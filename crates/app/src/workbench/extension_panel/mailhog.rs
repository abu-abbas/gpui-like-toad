//! Native presentation of the published MailHog view.

use super::*;

impl ExtensionPanel {
    // The published MailHog tree already carries the content and action IDs.
    // Present them in the native Inbox layout without changing the wire.
    pub(super) fn mailhog_layout(
        &self,
        tree: &Node,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let Node::Column { children } = tree else {
            return None;
        };
        match (self.slot, children.as_slice()) {
            (
                Slot::Explorer,
                [Node::Text {
                    text,
                    style: Some(TextStyle::Heading),
                    ..
                }, select @ Node::Select { .. }, search @ Node::Input { .. }, Node::Button {
                    id,
                    label,
                    disabled,
                    writes,
                    confirm,
                    ..
                }, list @ Node::List { .. }],
            ) => {
                let tool_icon = tools::tool_icon(store::ToolKind::MailHog, true, theme);
                let header = div()
                    .h(px(32.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_2))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tool_icon)
                    .child(text.as_str().to_owned());
                let refresh = self.action(
                    Action {
                        id,
                        item: None,
                        variant: None,
                        disabled: *disabled,
                        writes: *writes,
                        confirm: confirm.as_ref(),
                        button: explorer::control(id.as_str().to_owned(), theme)
                            .size(px(explorer::EXPLORER_CONTROL_HEIGHT))
                            .icon(icon(IconName::RefreshCw, 14., theme.muted_foreground))
                            .tooltip(label.as_str().to_owned()),
                    },
                    cx,
                );
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .min_h(px(0.))
                        .min_w(px(0.))
                        .overflow_hidden()
                        .bg(theme.sidebar)
                        .px(px(SPACE_2))
                        .py(px(6.))
                        .gap(px(6.))
                        .child(header)
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(self.render_node(select, theme, cx)),
                                )
                                .child(refresh),
                        )
                        .child(self.render_node(search, theme, cx))
                        .child(self.render_node(list, theme, cx))
                        .into_any_element(),
                )
            }
            (
                Slot::Main,
                [Node::Text { text: subject, .. }, Node::Text { text: sender, .. }, Node::Tabs { id, active, items }, Node::Row { children: actions }],
            ) => {
                let body = &items.iter().find(|item| &item.id == active)?.child;
                let mut toolbar = self
                    .tab_strip(id, active, items, theme, cx)
                    .flex_none()
                    .h(px(TOOLBAR_HEIGHT))
                    .px(px(SPACE_4))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1());
                if let Some(raw) = items.iter().find_map(|item| match &item.child {
                    Node::Code {
                        text, copy: true, ..
                    } => Some(text.as_str().to_owned()),
                    _ => None,
                }) {
                    toolbar = toolbar.child(
                        Button::new("mail-copy-raw")
                            .small()
                            .ghost()
                            .icon(IconName::Copy)
                            .tooltip("Copy raw source")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                    raw.clone(),
                                ))
                            }),
                    );
                }
                for action in actions {
                    let Node::Button {
                        id,
                        label,
                        disabled,
                        writes,
                        confirm,
                        ..
                    } = action
                    else {
                        toolbar = toolbar.child(self.render_node(action, theme, cx));
                        continue;
                    };
                    let button = Button::new(id.as_str().to_owned()).small().ghost();
                    let button = if *writes {
                        button
                            .icon(IconName::Trash)
                            .tooltip(label.as_str().to_owned())
                    } else {
                        button.label(label.as_str().to_owned())
                    };
                    toolbar = toolbar.child(self.action(
                        Action {
                            id,
                            item: None,
                            variant: None,
                            disabled: *disabled,
                            writes: *writes,
                            confirm: confirm.as_ref(),
                            button,
                        },
                        cx,
                    ));
                }
                let body = match body {
                    Node::Code { text, .. } | Node::Text { text, .. } => {
                        tools_mail::mail_body_text(
                            text.as_str().to_owned(),
                            matches!(body, Node::Code { .. }),
                            theme,
                        )
                        .into_any_element()
                    }
                    _ => self.render_node(body, theme, cx),
                };
                let headers = items
                    .iter()
                    .find_map(|item| match &item.child {
                        Node::Kv { rows } => Some(rows.as_slice()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let metadata = ["To", "Cc", "Date"].into_iter().filter_map(|label| {
                    let value = headers
                        .iter()
                        .find(|(key, _)| key.as_str().eq_ignore_ascii_case(label))?
                        .1
                        .as_str();
                    (!value.is_empty())
                        .then(|| tools_mail::mail_meta_row(label, value.to_owned(), theme))
                });
                let subject = if subject.as_str().is_empty() {
                    "(no subject)"
                } else {
                    subject.as_str()
                };
                Some(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .min_h(px(0.))
                        .min_w(px(0.))
                        .bg(theme.background)
                        .child(toolbar)
                        .child(
                            div()
                                .px(px(SPACE_4))
                                .pt(px(SPACE_4))
                                .pb(px(SPACE_3))
                                .flex()
                                .flex_col()
                                .gap(px(SPACE_1))
                                .child(
                                    div()
                                        .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                                        .line_height(crate::theme::ui_size(
                                            theme,
                                            LEADING_HEADING_MD,
                                        ))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(subject.to_owned()),
                                )
                                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                                .child(tools_mail::mail_meta_row(
                                    "From",
                                    sender.as_str().to_owned(),
                                    theme,
                                ))
                                .children(metadata),
                        )
                        .child(
                            div()
                                .id("extension-mail-body")
                                .flex_1()
                                .min_h(px(0.))
                                .overflow_y_scroll()
                                .px(px(SPACE_4))
                                .pb(px(SPACE_4))
                                .child(body),
                        )
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }
}
