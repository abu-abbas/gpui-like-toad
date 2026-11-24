//! Local diagnostics in Settings. Reports include both logs regardless of which tab is open.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LogTab {
    Errors,
    Crashes,
    /// Logs of the servers Kilat starts for Redis and MailHog.
    DevTools,
}

impl LogTab {
    fn path(self) -> PathBuf {
        match self {
            Self::Errors => crate::error_log::log_path(),
            Self::Crashes => crate::crash_log::log_path(),
            Self::DevTools => devtools::server::log_dir(&store::config_dir()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Errors => "Error log",
            Self::Crashes => "Crash log",
            Self::DevTools => "Developer Tools",
        }
    }
}

impl Workbench {
    pub(super) fn reload_settings_log(&mut self) {
        let tab = self.settings.log_tab;
        if tab == LogTab::DevTools {
            return self.reload_dev_logs();
        }
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
        for tab in [LogTab::Errors, LogTab::Crashes, LogTab::DevTools] {
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
                                this.settings.dev_log_confirm = false;
                                this.reload_settings_log();
                                cx.notify();
                            })),
                    ),
            );
        }

        if selected == LogTab::DevTools {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .gap(px(SPACE_2))
                .text_size(theme::ui_size(theme, FONT_LABEL))
                .child(caption_text(
                    "Output of the Redis and MailHog servers Kilat starts. New entries show local time with its UTC offset. Each log keeps about 5 MB plus one older part.".into(),
                    theme,
                ))
                .child(tabs)
                .child(self.render_dev_logs(theme, cx));
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
                        LogTab::Crashes | LogTab::DevTools => "No crashes logged.",
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

    /// Re-read the list of server logs and the tail of the one shown.
    pub(super) fn reload_dev_logs(&mut self) {
        let logs = devtools::server::list_logs(&store::config_dir());
        let keep = self
            .settings
            .dev_log
            .clone()
            .filter(|p| logs.iter().any(|l| &l.path == p));
        self.settings.dev_log = keep.or_else(|| logs.first().map(|l| l.path.clone()));
        self.settings.dev_log_text = self
            .settings
            .dev_log
            .as_deref()
            .map(|p| devtools::server::tail(p, DEV_LOG_SHOWN))
            .unwrap_or_default();
        self.settings.dev_logs = logs;
        // Newest output is at the end.
        self.settings.dev_log_scroll.scroll_to_bottom();
    }

    fn render_dev_logs(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let logs = &self.settings.dev_logs;
        let current = self
            .settings
            .dev_log
            .as_ref()
            .and_then(|p| logs.iter().find(|l| &l.path == p))
            .cloned();
        let view = cx.entity();
        let picker = {
            let logs = logs.clone();
            let chosen = current.as_ref().map(|l| l.path.clone());
            Button::new("dev-log-pick")
                .small()
                .ghost()
                .bg(theme.secondary)
                .min_w(px(220.))
                .disabled(logs.is_empty())
                .child(select_trigger(
                    current.as_ref().map_or_else(
                        || "No server logs".to_string(),
                        |l| format!("{}.log", l.name),
                    ),
                    theme,
                ))
                .dropdown_menu(move |menu, _, _| {
                    logs.iter().fold(menu.min_w(px(240.)), |menu, log| {
                        let view = view.clone();
                        let path = log.path.clone();
                        menu.item(
                            PopupMenuItem::new(format!(
                                "{}.log · {}",
                                log.name,
                                size_text(log.size)
                            ))
                            .checked(chosen.as_ref() == Some(&log.path))
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.settings.dev_log = Some(path.clone());
                                    this.settings.dev_log_confirm = false;
                                    this.reload_dev_logs();
                                    cx.notify();
                                })
                            }),
                        )
                    })
                })
        };
        let total: u64 = logs.iter().map(|l| l.size).sum();
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_2))
            .pb(px(SPACE_2))
            .border_b_1()
            .border_color(theme.border)
            .child(icon(IconName::FileText, 14., theme.muted_foreground))
            .child(picker)
            .children(
                current
                    .as_ref()
                    .map(|l| caption_text(size_text(l.size), theme)),
            )
            .child(div().flex_1())
            .when(logs.len() > 1, |d| {
                d.child(caption_text(
                    format!("{} logs · {}", logs.len(), size_text(total)),
                    theme,
                ))
            })
            .child(
                Button::new("refresh-dev-log")
                    .small()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reload_dev_logs();
                        cx.notify();
                    })),
            )
            .child({
                let text = self.settings.dev_log_text.clone();
                Button::new("copy-dev-log")
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .tooltip("Copy log")
                    .disabled(text.is_empty())
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.clone()))
                    })
            });
        let text = self.settings.dev_log_text.clone();
        let preview = div()
            .id("settings-dev-log-view")
            .track_scroll(&self.settings.dev_log_scroll)
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(if logs.is_empty() {
                caption_text(
                    "No server logs yet. Turn on \"Start server from Kilat\" on a Redis or MailHog connection; its output is saved here.".into(),
                    theme,
                )
            } else if text.trim().is_empty() {
                caption_text("This log is empty.".into(), theme)
            } else {
                div()
                    .p(px(SPACE_2))
                    .rounded(px(RADIUS_MD))
                    .bg(theme.secondary)
                    .font_family(theme.mono_font_family.clone())
                    .text_size(theme::ui_size(theme, FONT_CAPTION))
                    .whitespace_normal()
                    .child(text)
            });
        let folder = LogTab::DevTools.path();
        let confirm = self.settings.dev_log_confirm;
        let clear_target = current.as_ref().map(|l| (l.path.clone(), l.name.clone()));
        let mut footer = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_2))
            .pt(px(SPACE_2))
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("open-dev-log-folder")
                    .small()
                    .ghost()
                    .label("Open folder")
                    .icon(IconName::FolderOpen)
                    .tooltip("Reveal the Developer Tools log folder")
                    .on_click(move |_, _, cx| {
                        let _ = std::fs::create_dir_all(&folder);
                        cx.reveal_path(&folder);
                    }),
            )
            .child(div().flex_1());
        footer = if confirm {
            let name = clear_target
                .as_ref()
                .map(|t| t.1.clone())
                .unwrap_or_default();
            footer
                .child(caption_text(format!("Clear {name}.log?"), theme))
                .child(
                    Button::new("dev-log-clear-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.dev_log_confirm = false;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("dev-log-clear-yes")
                        .small()
                        .danger()
                        .label("Clear log")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some((path, _)) = &clear_target {
                                if let Err(err) = devtools::server::clear_log(path) {
                                    this.push_message(format!("Failed to clear the log: {err}"));
                                }
                            }
                            this.settings.dev_log_confirm = false;
                            this.reload_dev_logs();
                            cx.notify();
                        })),
                )
        } else {
            footer.child(
                Button::new("dev-log-clear")
                    .small()
                    .ghost()
                    .label("Clear log…")
                    .disabled(current.as_ref().is_none_or(|l| l.size == 0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.dev_log_confirm = true;
                        cx.notify();
                    })),
            )
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap(px(SPACE_2))
            .child(toolbar)
            .child(preview)
            .child(footer)
    }
}

/// How much of a server log Settings shows.
const DEV_LOG_SHOWN: u64 = 128 * 1024;

fn size_text(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / KB / KB)
    }
}
