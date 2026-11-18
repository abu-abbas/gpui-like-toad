use super::*;

impl Workbench {
    pub(super) fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            // Tab accepts the highlighted suggestion while the popup is
            // open. The editor only does that for Enter, so Tab (its
            // indent action) is caught on the way down and replayed as
            // Enter; with the popup closed it indents as usual.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                let open = this
                    .active_tab()
                    .editor
                    .read(cx)
                    .completion_menu_state()
                    .open;
                if open {
                    cx.stop_propagation();
                    window.dispatch_action(
                        Box::new(Enter {
                            secondary: false,
                            shift: false,
                        }),
                        cx,
                    );
                }
            }))
            .child(Editor::new(&self.active_tab().editor).flex_1())
    }
}
