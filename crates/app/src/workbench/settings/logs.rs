//! Settings > Logs: where errors and crashes are written, the newest
//! errors, and buttons to copy a report, open the folder or clear the log.

use super::*;

impl Workbench {
    pub(super) fn reload_error_log(&mut self) {
        self.settings.log_text = crate::error_log::recent_errors();
    }

    pub(super) fn render_logs(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let errors_path = crate::error_log::log_path();
        let crash_path = crate::crash_log::log_path();
        let text = &self.settings.log_text;
        let actions = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(SPACE_2))
            .child(
                Button::new("copy-error-report")
                    .label("Copy report")
                    .icon(IconName::Copy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                            crate::error_log::report(),
                        ));
                        this.push_message(
                            "Error report copied. Paste it into a chat or issue.".to_string(),
                        );
                        cx.notify();
                    })),
            )
            .child(
                Button::new("open-log-folder")
                    .label("Open folder")
                    .icon(IconName::FolderOpen)
                    .on_click(move |_, _, cx| {
                        let path = crate::error_log::log_path();
                        // Reveal the file when it exists, else its folder.
                        if path.exists() {
                            cx.reveal_path(&path);
                        } else if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                            cx.reveal_path(dir);
                        }
                    }),
            )
            .child(
                Button::new("refresh-error-log")
                    .label("Refresh")
                    .icon(IconName::RefreshCw)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reload_error_log();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("clear-error-log")
                    .label("Clear log")
                    .icon(IconName::Trash)
                    .disabled(text.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Err(err) = crate::error_log::clear() {
                            // Not through push_message: that would log it.
                            this.settings.log_text = format!("Failed to clear the log: {err}");
                        } else {
                            this.reload_error_log();
                        }
                        cx.notify();
                    })),
            );
        let view = div()
            .id("error-log-view")
            .h(px(300.))
            .overflow_y_scroll()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .p(px(SPACE_2))
            .font_family(theme.mono_font_family.clone())
            .text_size(theme::ui_size(theme, FONT_LABEL))
            .child(if text.is_empty() {
                div()
                    .text_color(theme.muted_foreground)
                    .child("No errors logged.")
            } else {
                // Newest first, like the Info panel.
                div()
                    .flex()
                    .flex_col()
                    .children(text.lines().rev().map(|line| {
                        div()
                            .text_color(if line.starts_with('[') {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child(line.to_string())
                    }))
            });
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(form_label("Error log", theme))
            .child(caption_text(
                "Errors shown in the Info panel are saved in this file so you can review them \
                 after closing the app. Use Copy Report to include them in a bug report."
                    .to_string(),
                theme,
            ))
            .child(caption_text(errors_path.display().to_string(), theme))
            .child(actions)
            .child(view)
            .child(div().h(px(SPACE_2)))
            .child(form_label("Crash log", theme))
            .child(caption_text(
                format!(
                    "Crash details are saved in {} and included when you select \
                     Copy Report.",
                    crash_path.display()
                ),
                theme,
            ))
    }
}
