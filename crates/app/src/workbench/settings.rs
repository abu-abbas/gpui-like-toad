use super::*;

mod appearance;
mod logs;

impl Workbench {
    /// Pick the color theme for light (`dark == false`) or dark mode;
    /// `None` is the app's own palette.
    fn set_theme_name(
        &mut self,
        dark: bool,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut names = theme::theme_names(cx);
        if dark {
            names.dark = name;
        } else {
            names.light = name;
        }
        theme::set_theme_names(names.clone(), cx);
        theme::apply_preference(theme::preference(cx), window, cx);
        if let Err(err) = store::save_theme_names(&names) {
            self.push_message(format!("Gagal menyimpan pilihan tema: {err}"));
        }
        cx.notify();
    }

    fn set_theme(&mut self, pref: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        theme::apply_preference(pref, window, cx);
        if let Err(err) = store::save_theme_preference(pref) {
            self.push_message(format!("Gagal menyimpan preferensi tema: {err}"));
        }
        cx.notify();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Appearance,
    Updates,
    Logs,
    About,
}

impl SettingsPage {
    const ALL: [Self; 4] = [Self::Appearance, Self::Updates, Self::Logs, Self::About];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Updates => "Updates",
            Self::Logs => "Logs",
            Self::About => "About Ora",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => {
                "appearance theme light dark system color accent font size motion icon"
            }
            Self::Updates => "updates check download version proxy network",
            Self::Logs => "logs log error crash report bug lapor",
            Self::About => "about kilat version",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Appearance => IconName::Sun,
            Self::Updates => IconName::Download,
            Self::Logs => IconName::FileText,
            Self::About => IconName::Info,
        }
    }
}

pub(super) struct SettingsState {
    pub(super) open: bool,
    page: SettingsPage,
    pub(super) search: Entity<InputState>,
    pub(super) proxy_input: Entity<InputState>,
    /// What Settings > Logs shows; read from disk when the page opens.
    pub(super) log_text: String,
    previous_focus: Option<gpui_kit::FocusHandle>,
}

impl SettingsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workbench>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let proxy = store::load_update_prefs().proxy.unwrap_or_default();
        let proxy_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://proxy.kantor:8080")
                .default_value(proxy)
        });
        Self {
            open: false,
            page: SettingsPage::Appearance,
            search,
            proxy_input,
            log_text: String::new(),
            previous_focus: None,
        }
    }
}

impl Workbench {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.open {
            return;
        }
        self.settings.previous_focus = window.focused(cx);
        self.settings.open = true;
        self.reload_error_log();
        Self::set_input(&self.settings.search, String::new(), window, cx);
        self.settings
            .search
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.open = false;
        let focus = self
            .settings
            .previous_focus
            .take()
            .unwrap_or_else(|| self.focus_handle.clone());
        focus.focus(window, cx);
        cx.notify();
    }

    fn update_appearance(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut store::AppearancePrefs),
    ) {
        let mut prefs = theme::appearance(cx);
        change(&mut prefs);
        if let Err(err) = store::save_appearance(&prefs) {
            self.push_message(format!("Gagal menyimpan pengaturan tampilan: {err}"));
        }
        theme::set_appearance(prefs, cx);
        theme::apply_preference(theme::preference(cx), window, cx);
        cx.notify();
    }

    pub(super) fn render_settings_dialog(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let filter = Self::input_text(&self.settings.search, cx).to_lowercase();
        let pages: Vec<_> = SettingsPage::ALL
            .into_iter()
            .filter(|page| page.keywords().contains(&filter))
            .collect();
        let page = pages
            .contains(&self.settings.page)
            .then_some(self.settings.page)
            .or_else(|| pages.first().copied());

        let mut nav = div()
            .flex()
            .flex_col()
            .flex_none()
            .h_full()
            .w(px(224.))
            .bg(theme.sidebar)
            .p(px(SPACE_3))
            .gap(px(SPACE_2))
            .child(
                div()
                    .px(px(SPACE_1))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(theme::ui_size(theme, FONT_HEADING_MD))
                    .child("Settings"),
            )
            .child(Input::new(&self.settings.search).small().w_full());
        for entry in pages {
            let selected = page == Some(entry);
            nav = nav.child(
                Button::new(entry.label())
                    .small()
                    .ghost()
                    .w_full()
                    .when(selected, |button| button.bg(theme.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.page = entry;
                        if entry == SettingsPage::Logs {
                            this.reload_error_log();
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .w_full()
                            .items_center()
                            .gap(px(SPACE_2))
                            .child(icon(
                                entry.icon(),
                                16.,
                                if selected {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                },
                            ))
                            .child(entry.label()),
                    ),
            );
        }

        let content = div()
            .id("settings-content")
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(SPACE_6))
                    .gap(px(SPACE_4))
                    .child(
                        div()
                            .pr(px(36.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(theme::ui_size(theme, FONT_HEADING_XL))
                            .child(page.map_or("No matching settings", SettingsPage::label)),
                    )
                    .children(match page {
                        Some(SettingsPage::Appearance) => {
                            Some(self.render_appearance(theme, cx).into_any_element())
                        }
                        Some(SettingsPage::Updates) => {
                            Some(self.render_update_settings(theme, cx).into_any_element())
                        }
                        Some(SettingsPage::Logs) => {
                            Some(self.render_logs(theme, cx).into_any_element())
                        }
                        Some(SettingsPage::About) => {
                            Some(crate::about::content(theme, cx).into_any_element())
                        }
                        None => None,
                    }),
            );
        div()
            .id("settings-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::black().opacity(0.32))
            .p(px(SPACE_4))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
            )
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_settings(window, cx);
                    }
                }),
            )
            .child(
                div()
                    .id("settings-dialog")
                    .relative()
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation()
                    })
                    .flex()
                    .size_full()
                    .max_w(px(920.))
                    .max_h(px(690.))
                    .overflow_hidden()
                    .rounded(px(12.))
                    .shadow_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(nav)
                    .child(content)
                    .child(
                        div().absolute().top(px(SPACE_3)).right(px(SPACE_3)).child(
                            Button::new("close-settings")
                                .small()
                                .ghost()
                                .icon(IconName::X)
                                .tooltip("Close Settings")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_settings(window, cx)
                                })),
                        ),
                    ),
            )
    }
}
