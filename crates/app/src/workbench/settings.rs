use super::*;

impl Workbench {
    /// One "Light theme" / "Dark theme" picker: Ora first, then the
    /// bundled gpui-kit themes for that mode.
    fn render_theme_picker(
        &self,
        dark: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let names = theme::theme_names(cx);
        let current = if dark { names.dark } else { names.light };
        let options = theme::theme_names_for(dark, cx);
        // A saved name that no longer exists shows (and acts) as Ora.
        let current = current.filter(|c| options.contains(c));
        let label = current
            .clone()
            .unwrap_or_else(|| theme::ORA_THEME_LABEL.to_string());
        let view = cx.entity();
        let id = if dark {
            "theme-dark-select"
        } else {
            "theme-light-select"
        };
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_1))
            .child(caption_text(
                if dark { "Dark theme" } else { "Light theme" }.to_string(),
                theme,
            ))
            .child(
                Button::new(id)
                    .child(select_trigger(label, theme))
                    .w_full()
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(220.));
                        let entries =
                            std::iter::once(None).chain(options.iter().cloned().map(Some));
                        for entry in entries {
                            let view = view.clone();
                            let checked = entry == current;
                            let text = entry
                                .clone()
                                .unwrap_or_else(|| theme::ORA_THEME_LABEL.to_string());
                            menu = menu.item(PopupMenuItem::new(text).checked(checked).on_click(
                                move |_, window, cx: &mut App| {
                                    let entry = entry.clone();
                                    view.update(cx, |this, cx| {
                                        this.set_theme_name(dark, entry, window, cx)
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
    }

    pub(super) fn render_settings_panel(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut panel = div().flex().flex_col().size_full().bg(theme.sidebar);
        // flex_none: the header keeps its height when the body is taller
        // than the panel; the body scrolls instead.
        panel = panel.child(self.render_panel_header("Settings", theme).flex_none());
        let mut body = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_2))
            .p(px(SPACE_3))
            .child(form_label("Appearance", theme));
        for (pref, icon_name, label) in [
            (ThemePreference::Light, IconName::Sun, "Light"),
            (ThemePreference::Dark, IconName::Moon, "Dark"),
            (ThemePreference::System, IconName::Monitor, "System"),
        ] {
            let selected = self.theme_pref == pref;
            let mut row = div()
                .id(format!("theme-{label}"))
                .flex()
                .flex_row()
                .items_center()
                .h(px(TOOLBAR_HEIGHT))
                .gap(px(SPACE_2))
                .px(px(SPACE_2))
                .rounded(px(RADIUS_MD))
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(pref, window, cx)));
            if selected {
                row = row.bg(theme.accent);
            } else {
                row = row.hover(|this| this.bg(theme.secondary));
            }
            let color = if selected {
                theme.accent_foreground
            } else {
                theme.foreground
            };
            row = row
                .child(icon(
                    icon_name,
                    16.,
                    if selected {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    },
                ))
                .child(
                    div()
                        .text_size(px(FONT_LABEL))
                        .line_height(px(LEADING_LABEL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .child(label),
                )
                .child(div().flex_1());
            if selected {
                row = row.child(icon(IconName::Check, 14., theme.primary));
            }
            body = body.child(row);
        }
        body = body
            .child(div().h(px(SPACE_2)))
            .child(self.render_theme_picker(false, theme, cx))
            .child(self.render_theme_picker(true, theme, cx))
            .child(caption_text(
                "Color themes bundled from gpui-kit. Light/Dark/System above decides which one is shown.".to_string(),
                theme,
            ))
            .child(div().h(px(SPACE_4)))
            .child(self.render_update_settings(theme, cx))
            .child(div().h(px(SPACE_4)))
            .child(form_label("About", theme))
            .child(body_text(
                format!(concat!(app_name!(), " {}"), env!("CARGO_PKG_VERSION")),
                theme,
            ))
            .child(caption_text(
                "Oracle database client built with GPUI + Rust.".to_string(),
                theme,
            ))
            .child(caption_text(
                "Theme preference, saved connections, and query history are stored in your user config directory. Passwords are only saved when you tick \"Save password\", and then in the OS keychain, never in these files.".to_string(),
                theme,
            ));
        panel.child(
            div()
                .id("settings-scroll")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(body),
        )
    }
}

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
        theme::apply_preference(self.theme_pref, window, cx);
        if let Err(err) = store::save_theme_names(&names) {
            self.push_message(format!("Gagal menyimpan pilihan tema: {err}"));
        }
        cx.notify();
    }

    fn set_theme(&mut self, pref: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_pref = pref;
        theme::apply_preference(pref, window, cx);
        if let Err(err) = store::save_theme_preference(pref) {
            self.push_message(format!("Gagal menyimpan preferensi tema: {err}"));
        }
        cx.notify();
    }
}
