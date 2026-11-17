//! Quick Open: Ctrl/Cmd+P opens a small palette over the workbench. Type
//! part of a table or view name in the current schema, pick with Up/Down
//! or the mouse, Enter opens its data tab.

use super::*;

/// Rows listed in the palette.
const QUICK_OPEN_ROWS: usize = 10;

pub(super) struct QuickOpen {
    input: Entity<InputState>,
    query: String,
    selected: usize,
    _sub: Subscription,
}

impl Workbench {
    /// Open the palette (or close it, when already open).
    pub(super) fn toggle_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick_open.is_some() {
            self.close_quick_open(cx);
            return;
        }
        if self.conn.session.is_none() || self.show_connection_form {
            self.conn.status = "Quick Open butuh koneksi yang aktif.".to_string();
            cx.notify();
            return;
        }
        let input =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Cari tabel atau view..."));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    if let Some(open) = this.quick_open.as_mut() {
                        open.query = text;
                        open.selected = 0;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.confirm_quick_open(None, window, cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.quick_open = Some(QuickOpen {
            input,
            query: String::new(),
            selected: 0,
            _sub: sub,
        });
        cx.notify();
    }

    pub(super) fn close_quick_open(&mut self, cx: &mut Context<Self>) {
        self.quick_open = None;
        cx.notify();
    }

    fn quick_open_hits(&self) -> Vec<SchemaObject> {
        let query = self.quick_open.as_ref().map_or("", |q| q.query.as_str());
        schema::quick_open_matches(&self.conn.objects, query, QUICK_OPEN_ROWS)
            .into_iter()
            .cloned()
            .collect()
    }

    fn move_quick_open(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.quick_open_hits().len();
        if let Some(open) = self.quick_open.as_mut() {
            if count > 0 {
                open.selected =
                    (open.selected as isize + delta).rem_euclid(count as isize) as usize;
            }
        }
        cx.notify();
    }

    /// Open the hit at `ix` (default: the highlighted one) in a data tab.
    fn confirm_quick_open(
        &mut self,
        ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hits = self.quick_open_hits();
        let ix = ix.or(self.quick_open.as_ref().map(|q| q.selected));
        let Some(object) = ix.and_then(|ix| hits.get(ix)) else {
            return;
        };
        let (owner, name) = (object.owner.clone(), object.name.clone());
        self.close_quick_open(cx);
        self.conn.selected_object = Some(name.clone());
        self.open_table_data(&owner, &name, window, cx);
    }

    pub(super) fn render_quick_open(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(open) = self.quick_open.as_ref() else {
            return div().into_any_element();
        };
        let hits = self.quick_open_hits();
        let mut list = div().flex().flex_col().p(px(SPACE_1));
        if hits.is_empty() {
            list = list.child(div().px(px(SPACE_3)).py(px(SPACE_2)).child(caption_text(
                "Tidak ada tabel atau view yang cocok.".to_string(),
                theme,
            )));
        }
        for (ix, object) in hits.iter().enumerate() {
            let selected = ix == open.selected;
            let icon_name =
                if matches!(object.kind, ObjectKind::View | ObjectKind::MaterializedView) {
                    IconName::Eye
                } else {
                    IconName::Table
                };
            let mut row = div()
                .id(("quick-open-row", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .h(px(TREE_ROW_HEIGHT))
                .px(px(SPACE_2))
                .rounded(px(RADIUS_SM))
                .text_size(px(FONT_LABEL))
                .line_height(px(LEADING_LABEL))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_quick_open(Some(ix), window, cx)
                }));
            row = if selected {
                row.bg(theme.accent).text_color(theme.accent_foreground)
            } else {
                row.text_color(theme.foreground)
                    .hover(|this| this.bg(theme.secondary))
            };
            list = list.child(
                row.child(icon(icon_name, 14., theme.muted_foreground))
                    .child(div().flex_1().truncate().child(object.name.clone()))
                    .child(caption_text(object.owner.clone(), theme)),
            );
        }
        let card = div()
            .id("quick-open")
            .flex()
            .flex_col()
            .w(px(460.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "down" => this.move_quick_open(1, cx),
                    "up" => this.move_quick_open(-1, cx),
                    "escape" => this.close_quick_open(cx),
                    _ => {}
                }
            }))
            .child(
                div()
                    .p(px(SPACE_2))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Input::new(&open.input).w_full()),
            )
            .child(list)
            .child(
                div()
                    .px(px(SPACE_3))
                    .py(px(SPACE_1))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(caption_text(
                        "Enter buka data · ↑↓ pilih · Esc tutup".to_string(),
                        theme,
                    )),
            );
        div()
            .id("quick-open-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(96.))
            .bg(gpui_kit::hsla(0., 0., 0., 0.35))
            .occlude()
            .on_click(cx.listener(|this, _, _, cx| this.close_quick_open(cx)))
            .child(card)
            .into_any_element()
    }
}
