//! Local diagnostics in Settings. Reports include both logs regardless of which tab is open.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LogTab {
    Errors,
    Crashes,
}

impl LogTab {
    fn path(self) -> PathBuf {
        match self {
            Self::Errors => crate::error_log::log_path(),
            Self::Crashes => crate::crash_log::log_path(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Errors => "Error log",
            Self::Crashes => "Crash log",
        }
    }
}

impl Workbench {
    pub(super) fn reload_settings_log(&mut self) {
        let tab = self.settings.log_tab;
        let text = crate::error_log::tail_of(&tab.path());
        self.settings.log_entries = crate::diagnostics::entries(&text, tab == LogTab::Crashes);
        self.settings.expanded_entry = None;
    }

    fn render_log_timeline(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut list = div().flex().flex_col().gap(px(SPACE_1));
        let mut previous_day = None;
        for (index, entry) in self.settings.log_entries.iter().enumerate() {
            let day = entry.timestamp.map(|ts| ts.date());
            if day != previous_day {
                if let Some(day) = day {
                    list = list.child(
                        form_label(&day.format("%b %-d, %Y").to_string(), theme).pt(px(SPACE_2)),
                    );
                }
                previous_day = day;
            }
            let expanded = self.settings.expanded_entry == Some(index);
            let summary = entry.summary();
            let row = Button::new(("settings-log-entry", index))
                .small()
                .ghost()
                .w_full()
                .accessibility_label(format!(
                    "{} {summary}",
                    if expanded { "Collapse" } else { "Expand" }
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings.expanded_entry = if expanded { None } else { Some(index) };
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .w_full()
                        .items_center()
                        .gap(px(SPACE_2))
                        .child(icon(IconName::CircleAlert, 14., theme.danger))
                        .children(entry.timestamp.map(|ts| {
                            caption_text(ts.format("%H:%M:%S").to_string(), theme)
                                .font_family(theme.mono_font_family.clone())
                                .flex_none()
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_left()
                                .text_size(theme::ui_size(theme, FONT_LABEL))
                                .child(summary.to_string()),
                        )
                        .child(icon(
                            if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            },
                            14.,
                            theme.muted_foreground,
                        )),
                );
            list = list.child(row);
            if expanded {
                list = list.child(
                    div()
                        .ml(px(15.))
                        .border_l_1()
                        .border_color(theme.border)
                        .pl(px(SPACE_3))
                        .child(
                            div()
                                .p(px(SPACE_2))
                                .rounded(px(RADIUS_MD))
                                .bg(theme.secondary)
                                .font_family(theme.mono_font_family.clone())
                                .text_size(theme::ui_size(theme, FONT_LABEL))
                                .child(entry.message.trim_end().to_string()),
                        ),
                );
            }
        }
        list
    }

    pub(super) fn render_logs(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let selected = self.settings.log_tab;
        let path = selected.path();
        let empty = self.settings.log_entries.is_empty();
        let mut tabs = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(theme.border);
        for tab in [LogTab::Errors, LogTab::Crashes] {
            tabs = tabs.child(
                div()
                    .when(tab == selected, |v| {
                        v.border_b_2().border_color(theme.primary)
                    })
                    .child(
                        Button::new(tab.label())
                            .small()
                            .ghost()
                            .label(tab.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings.log_tab = tab;
                                this.reload_settings_log();
                                cx.notify();
                            })),
                    ),
            );
        }

        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_2))
            .pb(px(SPACE_2))
            .border_b_1()
            .border_color(theme.border)
            .child(icon(IconName::FileText, 14., theme.muted_foreground))
            .child(
                div().flex_1().child(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
            )
            .child(
                Button::new("refresh-log")
                    .small()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reload_settings_log();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("copy-error-report")
                    .small()
                    .label("Copy report")
                    .tooltip("Copy recent errors and crashes to the clipboard")
                    .icon(IconName::Copy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                            crate::error_log::report(),
                        ));
                        this.push_message("Diagnostic report copied.".into());
                        cx.notify();
                    })),
            );

        let view = div()
            .id("settings-log-view")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(if empty {
                caption_text(
                    match selected {
                        LogTab::Errors => "No errors logged.",
                        LogTab::Crashes => "No crashes logged.",
                    }
                    .into(),
                    theme,
                )
            } else {
                self.render_log_timeline(theme, cx)
            });

        let footer = div()
            .flex()
            .items_center()
            .flex_none()
            .justify_between()
            .pt(px(SPACE_2))
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("open-log-folder")
                    .small()
                    .ghost()
                    .label("Open folder")
                    .icon(IconName::FolderOpen)
                    .tooltip("Reveal the selected log in its folder")
                    .on_click(move |_, _, cx| {
                        if path.exists() {
                            cx.reveal_path(&path);
                        } else if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                            cx.reveal_path(dir);
                        }
                    }),
            )
            .child(
                Button::new("clear-log")
                    .small()
                    .ghost()
                    .label("Clear log…")
                    .disabled(empty)
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let prompt = window.prompt(
                            gpui_kit::PromptLevel::Warning,
                            "Clear this log?",
                            Some(selected.label()),
                            &["Cancel", "Clear log"],
                            cx,
                        );
                        cx.spawn(async move |view, cx| {
                            if prompt.await != Ok(1) {
                                return;
                            }
                            let _ = view.update(cx, |this, cx| {
                                if let Err(err) = crate::error_log::clear(&selected.path()) {
                                    this.push_message(format!("Failed to clear the log: {err}"));
                                }
                                this.reload_settings_log();
                                cx.notify();
                            });
                        })
                        .detach();
                    })),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap(px(SPACE_2))
            .text_size(theme::ui_size(theme, FONT_LABEL))
            .child(caption_text(
                "Review errors and crashes saved on this device.".into(),
                theme,
            ))
            .child(tabs)
            .child(toolbar)
            .child(view)
            .child(footer)
    }
}
