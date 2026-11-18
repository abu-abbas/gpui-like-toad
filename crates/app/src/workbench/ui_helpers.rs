use super::*;

pub(super) fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

pub(super) fn status_dot(color: Hsla) -> gpui_kit::Div {
    div().size(px(8.)).rounded_full().bg(color).flex_none()
}

pub(super) fn caption_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_CAPTION))
        .line_height(px(LEADING_CAPTION))
        .text_color(theme.muted_foreground)
        .child(text)
}

pub(super) fn body_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_BODY))
        .line_height(px(LEADING_BODY))
        .text_color(theme.foreground)
        .child(text)
}

pub(super) fn form_label(text: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
        .line_height(px(LEADING_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(text.to_string())
}

pub(super) fn empty_state_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
        .line_height(px(LEADING_LABEL))
        .text_color(theme.muted_foreground)
        .child(text)
}

/// Content of a select-style dropdown button: the value left aligned and a
/// chevron on the right, like a native select (Button centers its label).
pub(super) fn select_trigger(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .min_w(px(0.))
        .gap(px(SPACE_2))
        .child(div().flex_1().min_w(px(0.)).truncate().child(text))
        .child(icon(IconName::ChevronDown, 14., theme.muted_foreground))
}
