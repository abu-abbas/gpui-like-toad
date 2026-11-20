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
        .line_height(crate::theme::ui_size(&theme, LEADING_CAPTION))
        .text_color(theme.muted_foreground)
        .child(text)
}

pub(super) fn body_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_BODY))
        .line_height(crate::theme::ui_size(&theme, LEADING_BODY))
        .text_color(theme.foreground)
        .child(text)
}

pub(super) fn form_label(text: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
        .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(text.to_string())
}

pub(super) fn empty_state_text(text: String, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
        .line_height(crate::theme::ui_size(&theme, LEADING_LABEL))
        .text_color(theme::empty_hint(theme))
        .child(text)
}

/// Centered spinner with a short caption, for areas whose content is
/// still being fetched. Same footprint as the empty-state text it replaces,
/// so the layout does not jump when the data arrives.
pub(super) fn loading_state(text: &str, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SPACE_2))
        .child(
            gpui_kit::component::spinner::Spinner::new()
                .large()
                .color(theme.primary),
        )
        .child(empty_state_text(text.to_string(), theme))
}

/// Dimmed cover with a spinner over content that is already on screen
/// (grid being reloaded). Absolute, so the content keeps its size.
pub(super) fn loading_overlay(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.background.opacity(0.55))
        .child(
            gpui_kit::component::spinner::Spinner::new()
                .large()
                .color(theme.primary),
        )
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

pub(super) fn environment_badge(
    access: ConnectionAccess,
    theme: &gpui_kit::component::Theme,
) -> Option<gpui_kit::Div> {
    let (label, color) = match access.environment {
        ConnectionEnvironment::Production => ("PROD", theme.danger),
        ConnectionEnvironment::Staging => ("STAGING", theme.warning),
        ConnectionEnvironment::Development => ("DEV", theme.success),
        ConnectionEnvironment::None if access.protect_production => ("PROD", theme.danger),
        ConnectionEnvironment::None if access.sysdba => ("SYSDBA", theme.warning),
        ConnectionEnvironment::None if access.read_only => ("READ ONLY", theme.muted_foreground),
        ConnectionEnvironment::None => return None,
    };
    Some(
        div()
            .flex_none()
            .px(px(4.))
            .rounded(px(RADIUS_SM))
            .bg(theme.background.blend(color.opacity(0.12)))
            .text_color(color)
            .text_size(theme::ui_size(theme, 10.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(label),
    )
}
