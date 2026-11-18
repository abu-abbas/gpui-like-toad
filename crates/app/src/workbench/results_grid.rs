//! Keep the table's deferred context menu and its submenus at the explorer label size.
use super::*;
use gpui_kit::component::FocusableExt as _;
use gpui_kit::{
    AnyElement, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId, Pixels,
};

pub(super) struct ResultsGrid {
    table: AnyElement,
    rem: Pixels,
}

impl ResultsGrid {
    pub(super) fn new(
        state: &Entity<TableState<ResultsDelegate>>,
        theme: &gpui_kit::component::Theme,
    ) -> Self {
        Self {
            table: DataTable::new(state)
                .xsmall()
                .header_row_height(px(DATA_HEADER_HEIGHT))
                .stripe(true)
                .fixed_cols_border(false)
                .bordered(false)
                .focus_ring(false)
                .into_any_element(),
            // PopupMenu uses text_sm (0.875rem), including for submenu triggers.
            rem: theme::ui_size(theme, FONT_LABEL) / 0.875,
        }
    }
}

impl IntoElement for ResultsGrid {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ResultsGrid {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (
            window.with_rem_size(Some(self.rem), |window| {
                self.table.request_layout(window, cx)
            }),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_rem_size(Some(self.rem), |window| self.table.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_rem_size(Some(self.rem), |window| self.table.paint(window, cx));
    }
}
