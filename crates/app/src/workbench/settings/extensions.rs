//! Settings > Extensions: switch the optional databases and developer
//! tools on and off. Stage 1: every extension is bundled in Kilat, so
//! Install / Uninstall only toggles it; saved connections, tools and
//! passwords are never deleted.

use super::appearance::setting_group;
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtFilter {
    All,
    Installed,
    Databases,
    Tools,
}

impl ExtFilter {
    const ALL: [Self; 4] = [Self::All, Self::Installed, Self::Databases, Self::Tools];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Installed => "Installed",
            Self::Databases => "Databases",
            Self::Tools => "Developer tools",
        }
    }

    fn matches(self, info: &store::ExtensionInfo, installed: bool) -> bool {
        match self {
            Self::All => true,
            Self::Installed => installed,
            Self::Databases => info.kind == store::ExtensionKind::Database,
            Self::Tools => info.kind == store::ExtensionKind::Tool,
        }
    }
}

impl Workbench {
    fn set_extension_installed(&mut self, id: &'static str, install: bool, cx: &mut Context<Self>) {
        if install {
            self.extensions.install(id);
        } else {
            self.extensions.uninstall(id);
        }
        if let Err(err) = store::save_extensions(&self.extensions) {
            self.push_message(format!("Could not save extensions: {err}"));
        }
        cx.notify();
    }

    pub(super) fn render_extensions(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let filter = self.settings.ext_filter;
        let tabs = ExtFilter::ALL.into_iter().fold(
            div().flex().flex_row().gap(px(SPACE_2)),
            |row, tab| {
                row.child(
                    Button::new(tab.label())
                        .small()
                        .ghost()
                        .label(tab.label())
                        .selected(tab == filter)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.ext_filter = tab;
                            cx.notify();
                        })),
                )
            },
        );
        let rows: Vec<_> = store::REGISTRY
            .iter()
            .enumerate()
            .filter(|(_, info)| filter.matches(info, self.extensions.is_installed(info.id)))
            .map(|(ix, info)| {
                let installed = self.extensions.is_installed(info.id);
                let id = info.id;
                let control = if info.built_in {
                    caption_text("Built in".into(), theme).into_any_element()
                } else {
                    Button::new(("ext", ix))
                        .small()
                        .label(if installed { "Uninstall" } else { "Install" })
                        .when(!installed, |b| b.primary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_extension_installed(id, !installed, cx)
                        }))
                        .into_any_element()
                };
                setting_row(info.name, info.description, control, theme)
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .child(tabs)
            .child(if rows.is_empty() {
                div()
                    .child(caption_text("Nothing here yet.".into(), theme))
                    .into_any_element()
            } else {
                setting_group(theme, rows).into_any_element()
            })
            .child(caption_text(
                "Uninstalling keeps your saved connections and passwords; they open again once the extension is back.".into(),
                theme,
            ))
    }
}
