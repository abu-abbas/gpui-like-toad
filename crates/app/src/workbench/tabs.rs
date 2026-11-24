use super::*;

impl Workbench {
    pub(super) fn render_tab_bar(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Tabs scroll sideways while the add button stays pinned on the right.
        let view = cx.weak_entity();
        let scroll = self.tab_scroll.clone();
        let was_overflowing = self.tab_overflow;
        const SCROLL_CONTROLS_WIDTH: f32 = 52.;
        let overflow_threshold = px(if was_overflowing {
            SCROLL_CONTROLS_WIDTH + SPACE_1
        } else {
            0.
        });
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink(1.)
            .min_w(px(0.))
            .h_full()
            .gap(px(SPACE_1))
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        for (ix, tab) in self.tabs.iter().enumerate() {
            let active = !self.show_empty_workspace && ix == self.active_tab;
            strip = strip.child(self.render_query_tab(ix, tab, active, theme, cx));
        }
        // Always the last tab, after every query/data tab.
        if self.show_whats_new_tab() {
            strip = strip.child(self.render_whats_new_tab(theme, cx));
        }
        let strip = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .on_children_prepainted(move |_, window, cx| {
                let overflowing = scroll.max_offset().x > overflow_threshold;
                if overflowing != was_overflowing {
                    let view = view.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = view.update(cx, |this, cx| {
                            if this.tab_overflow != overflowing {
                                this.tab_overflow = overflowing;
                                cx.notify();
                            }
                        });
                    });
                }
            })
            .child(strip);
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(TAB_BAR_HEIGHT))
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .child(strip)
            .child(
                div()
                    .id("tab-add")
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(28.))
                    .w(px(28.))
                    .rounded(px(RADIUS_MD))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx)))
                    .child(icon(IconName::Plus, 16., theme.muted_foreground)),
            )
            .when(self.tab_overflow, |bar| {
                let mut controls = div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .w(px(SCROLL_CONTROLS_WIDTH));
                for (id, symbol, delta) in [
                    ("tabs-scroll-left", IconName::ChevronLeft, 150.),
                    ("tabs-scroll-right", IconName::ChevronRight, -150.),
                ] {
                    controls = controls.child(
                        Button::new(id)
                            .small()
                            .ghost()
                            .icon(symbol)
                            .w(px(26.))
                            .tooltip(if delta > 0. {
                                "Scroll tabs left"
                            } else {
                                "Scroll tabs right"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut offset = this.tab_scroll.offset();
                                offset.x = (offset.x + px(delta))
                                    .clamp(-this.tab_scroll.max_offset().x, px(0.));
                                this.tab_scroll.set_offset(offset);
                                cx.notify();
                            })),
                    );
                }
                bar.child(controls)
            })
    }

    fn render_query_tab(
        &self,
        ix: usize,
        tab: &QueryTab,
        active: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The label area and the close button are siblings, not nested
        // click targets, so closing a tab never also activates it.
        let mut container = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(28.))
            .rounded(px(RADIUS_MD));
        let view = cx.entity();
        let tab_count = self.tabs.len();
        if active {
            container = container
                .bg(theme.tab_active)
                .border_1()
                .border_color(theme.border);
        } else {
            container = container.hover(|this| this.bg(theme.secondary));
        }
        let label_color = if active {
            theme.tab_active_foreground
        } else {
            theme.tab_foreground
        };
        container
            .context_menu(move |menu, _, _| {
                let close = view.clone();
                let others = view.clone();
                let right = view.clone();
                let saved = view.clone();
                let all = view.clone();
                menu.min_w(px(200.))
                    .item(
                        PopupMenuItem::new("Close").on_click(move |_, _, cx: &mut App| {
                            close.update(cx, |this, cx| {
                                this.request_close_where(|i, _| i == ix, cx)
                            });
                        }),
                    )
                    .item(
                        PopupMenuItem::new("Close Others")
                            .disabled(tab_count < 2)
                            .on_click(move |_, _, cx: &mut App| {
                                others.update(cx, |this, cx| {
                                    this.request_close_where(|i, _| i != ix, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Close to the Right")
                            .disabled(ix + 1 >= tab_count)
                            .on_click(move |_, _, cx: &mut App| {
                                right.update(cx, |this, cx| {
                                    this.request_close_where(|i, _| i > ix, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Close Saved").on_click(move |_, _, cx: &mut App| {
                            saved.update(cx, |this, cx| {
                                this.close_tabs_where(|_, t, cx| Self::tab_is_saved(t, cx), cx)
                            });
                        }),
                    )
                    .item(
                        PopupMenuItem::new("Close All").on_click(move |_, _, cx: &mut App| {
                            all.update(cx, |this, cx| this.request_close_where(|_, _| true, cx));
                        }),
                    )
            })
            .child(
                div()
                    .id(format!("query-tab-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_1()
                    .h(relative(1.0))
                    .gap(px(SPACE_1))
                    .pl(px(SPACE_2))
                    .pr(px(SPACE_1))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_tab(ix, cx)))
                    .child(icon(
                        match tab.kind {
                            TabKind::Query => IconName::Database,
                            TabKind::Data => IconName::Table,
                            TabKind::Script => IconName::FileCode,
                        },
                        14.,
                        if active {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        },
                    ))
                    .child({
                        let label = div()
                            .flex()
                            .flex_row()
                            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                            .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(label_color);
                        // Two tables with the same name in different
                        // databases differ in the first part, so show it in
                        // the accent colour and bold.
                        let split = tab
                            .data
                            .as_ref()
                            .and_then(|d| d.database.as_deref())
                            .and_then(|db| tab.title.strip_prefix(db))
                            .filter(|rest| rest.starts_with('.'));
                        match split {
                            Some(rest) => {
                                let db = &tab.title[..tab.title.len() - rest.len()];
                                label
                                    .child(
                                        div()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.primary)
                                            .child(db.to_string()),
                                    )
                                    .child(rest.to_string())
                            }
                            None => label.child(tab.title.clone()),
                        }
                    })
                    .children(
                        tab.conn
                            .and_then(|id| self.conn_by_id(id))
                            .and_then(|conn| ui_helpers::environment_badge(conn.access(), theme)),
                    )
                    // Which connection the tab runs on, once more than one
                    // is open (or "closed" when its connection is gone).
                    .when_some(self.tab_conn_label(tab), |this, (label, closed)| {
                        this.child(
                            div()
                                .flex_none()
                                .max_w(px(96.))
                                .truncate()
                                .px(px(SPACE_1))
                                .rounded(px(RADIUS_SM))
                                .bg(if closed {
                                    theme.danger.opacity(0.15)
                                } else {
                                    theme.secondary
                                })
                                .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                                .line_height(crate::theme::ui_size(theme, LEADING_CAPTION))
                                .text_color(if closed {
                                    theme.danger
                                } else {
                                    theme.muted_foreground
                                })
                                .child(label),
                        )
                    }),
            )
            .child(
                div()
                    .id(format!("query-tab-close-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .h(px(20.))
                    .w(px(20.))
                    .mr(px(SPACE_1))
                    .rounded(px(RADIUS_SM))
                    .hover(|this| this.bg(theme.danger).text_color(theme.danger_foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.request_close_where(|i, _| i == ix, cx)
                    }))
                    .child(icon(IconName::X, 12., theme.muted_foreground)),
            )
    }
}

impl Workbench {
    pub(super) fn active_tab(&self) -> &QueryTab {
        self.tabs.get(self.active_tab).unwrap_or(&self.scratch_tab)
    }

    pub(super) fn active_tab_mut(&mut self) -> &mut QueryTab {
        if self.active_tab < self.tabs.len() {
            &mut self.tabs[self.active_tab]
        } else {
            &mut self.scratch_tab
        }
    }

    pub(super) fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_tab_id += 1;
        let id = self.next_tab_id;
        let completion = self.completion.clone();
        let mut tab = QueryTab::new(id, format!("Query {id}"), "", &completion, window, cx);
        tab.conn = self.conn.is_open().then_some(self.conn.id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_empty_workspace = false;
        self.leave_whats_new();
        self.tab_scroll.scroll_to_item(self.active_tab);
        cx.notify();
    }

    /// Close the tabs picked by `pick(ix, tab)`. SQL tabs with unsaved
    /// text first raise the Save / Don't Save / Cancel dialog; closing
    /// the last tab returns to the empty workspace.
    pub(super) fn request_close_where(
        &mut self,
        pick: impl Fn(usize, &QueryTab) -> bool,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<u64> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(ix, t)| pick(*ix, t))
            .map(|(_, t)| t.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let scripts: Vec<(u64, String)> = self
            .tabs
            .iter()
            .filter(|t| ids.contains(&t.id) && t.has_unsaved_script(cx))
            .map(|t| (t.id, t.title.clone()))
            .collect();
        if scripts.is_empty() {
            self.close_tabs_where(|_, t, _| ids.contains(&t.id), cx);
            return;
        }
        self.disconnect_prompt = Some(conns::DisconnectPrompt::close_tabs(ids, scripts));
        cx.notify();
    }

    /// Close every tab for which `close(ix, tab)` is true. The active tab
    /// stays active when it survives; otherwise the nearest survivor is
    /// shown (or the workspace is empty when none is left).
    pub(super) fn close_tabs_where(
        &mut self,
        close: impl Fn(usize, &QueryTab, &App) -> bool,
        cx: &mut Context<Self>,
    ) {
        let active_id = self.tabs.get(self.active_tab).map(|t| t.id);
        let old_active = self.active_tab;
        let doomed: Vec<bool> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, t)| close(ix, t, cx))
            .collect();
        let mut ix = 0;
        self.tabs.retain(|_| {
            let keep = !doomed[ix];
            ix += 1;
            keep
        });
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.show_empty_workspace = true;
        } else if let Some(pos) = self.tabs.iter().position(|t| Some(t.id) == active_id) {
            self.active_tab = pos;
        } else {
            self.active_tab = old_active.min(self.tabs.len() - 1);
        }
        self.follow_tab_conn(cx);
        cx.notify();
    }

    /// Whether a tab holds nothing that would be lost: a data tab with
    /// no staged or uncommitted cell edits, or an empty SQL editor.
    fn tab_is_saved(tab: &QueryTab, cx: &App) -> bool {
        match &tab.data {
            Some(d) => d.edits.is_empty() && !d.has_staged(),
            None => !tab.has_unsaved_script(cx),
        }
    }

    pub(super) fn activate_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = ix;
        self.show_empty_workspace = false;
        self.leave_whats_new();
        self.tab_scroll.scroll_to_item(ix);
        self.follow_tab_conn(cx);
        cx.notify();
    }

    pub(super) fn new_tab_with_sql(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_tab(window, cx);
        let editor = self.active_tab().editor.clone();
        self.active_tab_mut().saved_text = sql.clone();
        editor.update(cx, |state, cx| state.set_value(sql, window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn closing_a_tab_with_unsaved_sql_asks_first(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(Workbench::new);
        window
            .update(cx, |this, window, cx| {
                this.add_tab(window, cx);
                let id = this.active_tab().id;
                // Empty tab: closes without a dialog.
                this.request_close_where(|i, _| i == 0, cx);
                assert!(this.disconnect_prompt.is_none());
                assert!(this.tabs.is_empty());

                this.add_tab(window, cx);
                let id2 = this.active_tab().id;
                assert_ne!(id, id2);
                let editor = this.active_tab().editor.clone();
                editor.update(cx, |s, cx| s.set_value("select 1 from dual", window, cx));
                this.request_close_where(|i, _| i == 0, cx);
                assert!(this.disconnect_prompt.is_some(), "unsaved text must prompt");
                assert_eq!(this.tabs.len(), 1);

                // Once saved, closing is silent.
                this.disconnect_prompt = None;
                this.tabs[0].saved_text = "select 1 from dual".to_string();
                this.request_close_where(|i, _| i == 0, cx);
                assert!(this.disconnect_prompt.is_none());
                assert!(this.tabs.is_empty());
            })
            .unwrap();
    }

    #[gpui_kit::test]
    fn finds_existing_data_tab_only_on_same_connection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(Workbench::new);
        window
            .update(cx, |this, window, cx| {
                this.add_tab(window, cx);
                let data = DataTab::new(None, "MAIN", "TASK_LOGS", ObjectKind::Table, window, cx);
                let tab = this.active_tab_mut();
                tab.conn = Some(7);
                tab.data = Some(data);
                assert_eq!(this.find_data_tab(7, None, "MAIN", "TASK_LOGS"), Some(0));
                // Another connection, schema or table must not match.
                assert_eq!(this.find_data_tab(8, None, "MAIN", "TASK_LOGS"), None);
                assert_eq!(this.find_data_tab(7, None, "OTHER", "TASK_LOGS"), None);
                assert_eq!(
                    this.find_data_tab(7, None, "MAIN", "TASK_LOG_COMMITS"),
                    None
                );
                // Nor a database: a tab without one is not the same table
                // as one in a named database.
                assert_eq!(
                    this.find_data_tab(7, Some("dev"), "MAIN", "TASK_LOGS"),
                    None
                );
            })
            .unwrap();
    }
}
