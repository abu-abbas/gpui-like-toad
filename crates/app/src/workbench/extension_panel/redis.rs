//! Native presentation of the published Redis views: the key browser in the
//! explorer column and the Value / Console tabs in the main column. The
//! trees carry the content and action IDs; this only lays them out.

use super::*;

impl ExtensionPanel {
    pub(super) fn redis_layout(
        &self,
        tree: &Node,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        match (self.slot, tree) {
            (Slot::Explorer, Node::Column { children }) => {
                let [Node::Text { text, .. }, db @ Node::Select { .. }, kind @ Node::Select { .. }, Node::Input { id: pattern, .. }, Node::Button {
                    id,
                    label,
                    disabled,
                    writes,
                    confirm,
                    ..
                }, rest @ ..] = children.as_slice()
                else {
                    return None;
                };
                let list = rest.iter().find(|node| matches!(node, Node::List { .. }))?;
                let tool_icon = tools::tool_icon(store::ToolKind::Redis, true, theme);
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
                let search = Input::new(&self.fields.get(pattern)?.input)
                    .small()
                    .prefix(icon(IconName::Search, 14., theme.muted_foreground))
                    .w_full();
                let notes = rest.iter().filter_map(|node| match node {
                    Node::Text {
                        text,
                        tone: Some(Tone::Danger),
                        ..
                    } => Some(
                        div()
                            .px(px(SPACE_2))
                            .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                            .text_color(theme.danger)
                            .child(text.as_str().to_owned()),
                    ),
                    _ => None,
                });
                let footer = rest.iter().rev().find_map(|node| match node {
                    Node::Text {
                        text,
                        tone: Some(Tone::Muted),
                        ..
                    } => Some(text.as_str().to_owned()),
                    _ => None,
                });
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
                                        .child(self.render_node(db, theme, cx)),
                                )
                                .child(refresh),
                        )
                        .child(self.render_node(kind, theme, cx))
                        .child(search)
                        .children(notes)
                        .child(self.render_node(list, theme, cx))
                        .children(footer.map(|text| {
                            div()
                                .flex_none()
                                .px(px(SPACE_2))
                                .child(caption_text(text, theme))
                        }))
                        .into_any_element(),
                )
            }
            (Slot::Main, Node::Tabs { id, active, items }) => {
                let body = &items.iter().find(|item| &item.id == active)?.child;
                let toolbar = self
                    .tab_strip(id, active, items, theme, cx)
                    .flex_none()
                    .h(px(TOOLBAR_HEIGHT))
                    .px(px(SPACE_4))
                    .border_b_1()
                    .border_color(theme.border);
                // The console prompt and input sit under the log, not in the scroll.
                let (prompt_rows, rest): (Vec<&Node>, Vec<&Node>) = match body {
                    Node::Column { children } => children.iter().partition(|node| {
                        matches!(node, Node::Row { children }
                            if matches!(children.as_slice(), [Node::Text { .. }, Node::Input { .. }]))
                    }),
                    other => (Vec::new(), vec![other]),
                };
                let content = div()
                    .id("extension-redis-body")
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(SPACE_4))
                    .children(
                        rest.into_iter()
                            .map(|node| self.render_node(node, theme, cx)),
                    );
                let prompt = prompt_rows.first().and_then(|node| match node {
                    Node::Row { children } => match children.as_slice() {
                        [Node::Text { text, .. }, input @ Node::Input { .. }] => {
                            Some((text.as_str().to_owned(), input))
                        }
                        _ => None,
                    },
                    _ => None,
                });
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .bg(theme.background)
                        .child(toolbar)
                        .child(content)
                        .children(prompt.map(|(prompt, input)| {
                            div()
                                .flex()
                                .items_center()
                                .flex_none()
                                .gap(px(SPACE_2))
                                .px(px(SPACE_4))
                                .py(px(SPACE_2))
                                .border_t_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .flex_none()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.primary)
                                        .child(prompt),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(self.render_node(input, theme, cx)),
                                )
                        }))
                        .into_any_element(),
                )
            }
            _ => None,
        }
    }
}
