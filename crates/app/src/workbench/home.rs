use super::*;

#[derive(Default)]
pub(super) struct HomeViewState {
    /// Show saved connections as a zebra list instead of cards.
    list: bool,
    connections_folded: bool,
    recent_folded: bool,
}

impl Workbench {
    pub(super) fn render_home(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_6))
            .p(px(SPACE_6))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1))
                    .child(
                        div()
                            .text_size(px(FONT_HEADING_XL))
                            .line_height(px(LEADING_HEADING_XL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(app_name!()),
                    )
                    .child(caption_text(
                        "Desktop Database Client · Built with GPUI + Rust".to_string(),
                        theme,
                    )),
            );
        content = content.child(self.render_home_connections(theme, cx));
        content = content.child(self.render_home_recent(theme, cx));
        div()
            .flex_1()
            .id("home-scroll")
            .overflow_y_scroll()
            .child(content)
    }

    /// Navicat-style start page: one tile per connection, plus the demo
    /// database and a "New connection" tile.
    fn render_home_connections(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let folded = self.home_view.connections_folded;
        let list_mode = self.home_view.list;
        let toggle = |id: &str, label: &'static str, icon_name: IconName, active: bool| {
            Button::new(id.to_string())
                .small()
                .ghost()
                .icon(icon_name)
                .selected(active)
                .tooltip(label)
        };
        let section = div().flex().flex_col().gap(px(SPACE_2)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_1))
                .child(self.section_title(
                    "home-conn-fold",
                    "Connections",
                    folded,
                    theme,
                    cx,
                    |this| {
                        this.home_view.connections_folded = !this.home_view.connections_folded;
                    },
                ))
                .child(div().flex_1())
                .when(!folded, |row| {
                    row.child(
                        toggle(
                            "home-view-cards",
                            "Tampilan kartu",
                            IconName::LayoutGrid,
                            !list_mode,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.home_view.list = false;
                            cx.notify();
                        })),
                    )
                    .child(
                        toggle(
                            "home-view-list",
                            "Tampilan daftar",
                            IconName::List,
                            list_mode,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.home_view.list = true;
                            cx.notify();
                        })),
                    )
                }),
        );
        if folded {
            return section;
        }
        if list_mode {
            return section.child(self.render_home_connection_list(theme, cx));
        }
        let tile = |id: String| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .w(px(232.))
                .max_w_full()
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .hover(|this| this.border_color(theme.primary))
        };
        let tile_title = |icon_name: IconName, title: String, subtitle: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_2))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(32.))
                        .rounded(px(RADIUS_MD))
                        .bg(theme.accent)
                        .child(icon(icon_name, 18., theme.primary)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .child(
                            div()
                                .text_size(px(FONT_BODY))
                                .line_height(px(LEADING_BODY))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .truncate()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(FONT_CAPTION))
                                .line_height(px(LEADING_CAPTION))
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(subtitle),
                        ),
                )
        };
        let mut grid = div().flex().flex_row().flex_wrap().gap(px(SPACE_3));
        // Green "Connected" line on tiles whose connection is open, with
        // "aktif" on the one the explorer shows.
        let open_badge = |id: Option<u64>| {
            id.map(|id| {
                let active = id == self.conn.id;
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_1))
                    .child(status_dot(theme.success))
                    .child(caption_text(
                        if active {
                            "Connected · aktif".to_string()
                        } else {
                            "Connected".to_string()
                        },
                        theme,
                    ))
            })
        };
        // An open connection's tile: switch to it (Buka) or close it.
        let open_actions =
            |key: String, id: u64, cx: &mut Context<Self>| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new(format!("{key}-open"))
                            .flex_1()
                            .label("Buka")
                            .icon(IconName::LogIn)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.switch_conn(id, window, cx)
                            })),
                    )
                    .child(
                        Button::new(format!("{key}-disconnect"))
                            .label("Disconnect")
                            .icon(IconName::LogOut)
                            .disabled(self.conn_by_id(id).is_some_and(|c| c.busy))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request_disconnect(id, window, cx)
                            })),
                    )
            };
        let demo_open = self.find_open_conn(None, true);
        if self.show_demo || demo_open.is_some() {
            grid = grid.child(
                tile("home-demo-tile".to_string())
                    .child(tile_title(
                        IconName::Database,
                        "Demo database".to_string(),
                        "Built-in, no Oracle needed".to_string(),
                    ))
                    .children(open_badge(demo_open))
                    .child(match demo_open {
                        Some(id) => {
                            open_actions("home-demo".to_string(), id, cx).into_any_element()
                        }
                        None => Button::new("home-demo")
                            .label("Connect")
                            .icon(IconName::Play)
                            .primary()
                            .disabled(self.conn.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.connect(true, window, cx)),
                            )
                            .into_any_element(),
                    }),
            );
        }
        for (ix, profile) in self.profiles.iter().enumerate() {
            let profile = profile.clone();
            let open = self.find_open_conn(Some(ix), false);
            grid = grid.child(
                tile(format!("home-profile-{ix}"))
                    .child(tile_title(
                        IconName::Database,
                        profile.name.clone(),
                        format!("{}@{}", profile.username, profile.connect_string),
                    ))
                    .children(open_badge(open))
                    .child(if let Some(id) = open {
                        open_actions(format!("home-use-{ix}"), id, cx).into_any_element()
                    } else {
                        Button::new(format!("home-use-{ix}"))
                            .label("Open")
                            .icon(IconName::LogIn)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // Saved password: connect now. Otherwise
                                // open the filled dialog for the password.
                                this.open_profile(ix, window, cx);
                            }))
                            .into_any_element()
                    }),
            );
        }
        grid = grid.child(
            tile("home-new-tile".to_string())
                .child(tile_title(
                    IconName::Plus,
                    "New connection".to_string(),
                    "Oracle (Easy Connect / TNS)".to_string(),
                ))
                .child(
                    Button::new("home-new")
                        .label("Create")
                        .icon(IconName::Plus)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_connection(window, cx);
                        })),
                ),
        );
        section.child(grid)
    }

    /// A section title with a fold chevron, like the Agents/Projects
    /// groups of a sidebar.
    fn section_title(
        &self,
        id: &'static str,
        label: &'static str,
        folded: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
        toggle: fn(&mut Workbench),
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_1))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                toggle(this);
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(FONT_HEADING_MD))
                    .line_height(px(LEADING_HEADING_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(label),
            )
            .child(icon(
                if folded {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                },
                14.,
                theme.muted_foreground,
            ))
    }

    /// Connections as a zebra table: status dot, name, user@target, action.
    fn render_home_connection_list(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let demo_open = self.find_open_conn(None, true);
        let row = |ix: usize| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(SPACE_3))
                .h(px(TOOLBAR_HEIGHT + 4.))
                .px(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .when(ix % 2 == 0, |r| r.bg(theme.table_even))
        };
        let cell = |text: String, color: gpui_kit::Hsla, weight: FontWeight, flex: f32| {
            div()
                .flex_basis(px(0.))
                .flex_grow(1.)
                .flex_shrink(1.)
                .min_w(px(0.))
                .w(relative(flex))
                .truncate()
                .text_size(px(FONT_LABEL))
                .line_height(px(LEADING_LABEL))
                .font_weight(weight)
                .text_color(color)
                .child(text)
        };
        let mut list = div().flex().flex_col().w_full();
        // Open connections get Buka + Disconnect, closed ones one button.
        let actions = |key: String,
                       open: Option<u64>,
                       closed: Button,
                       cx: &mut Context<Self>|
         -> gpui_kit::AnyElement {
            match open {
                Some(id) => div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .child(
                        Button::new(format!("{key}-open"))
                            .small()
                            .label("Buka")
                            .icon(IconName::LogIn)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.switch_conn(id, window, cx)
                            })),
                    )
                    .child(
                        Button::new(format!("{key}-disconnect"))
                            .small()
                            .label("Disconnect")
                            .icon(IconName::LogOut)
                            .disabled(self.conn_by_id(id).is_some_and(|c| c.busy))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request_disconnect(id, window, cx)
                            })),
                    )
                    .into_any_element(),
                None => closed.into_any_element(),
            }
        };
        let mut n = 1;
        if self.show_demo || demo_open.is_some() {
            list = list.child(
                row(n)
                    .child(status_dot(if demo_open.is_some() {
                        theme.success
                    } else {
                        theme.muted_foreground
                    }))
                    .child(cell(
                        "Demo database".to_string(),
                        theme.foreground,
                        FontWeight::SEMIBOLD,
                        0.3,
                    ))
                    .child(cell(
                        "Built-in, no Oracle needed".to_string(),
                        theme.muted_foreground,
                        FontWeight::NORMAL,
                        0.5,
                    ))
                    .child(actions(
                        "list-demo".to_string(),
                        demo_open,
                        Button::new("list-demo")
                            .small()
                            .label("Connect")
                            .icon(IconName::Play)
                            .disabled(self.conn.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.connect(true, window, cx)),
                            ),
                        cx,
                    )),
            );
            n += 1;
        }
        for (ix, profile) in self.profiles.iter().enumerate() {
            let open = self.find_open_conn(Some(ix), false);
            list = list.child(
                row(n)
                    .child(status_dot(if open.is_some() {
                        theme.success
                    } else {
                        theme.muted_foreground
                    }))
                    .child(cell(
                        profile.name.clone(),
                        theme.foreground,
                        FontWeight::SEMIBOLD,
                        0.3,
                    ))
                    .child(cell(
                        format!("{}@{}", profile.username, profile.connect_string),
                        theme.muted_foreground,
                        FontWeight::NORMAL,
                        0.5,
                    ))
                    .child(actions(
                        format!("list-use-{ix}"),
                        open,
                        Button::new(format!("list-use-{ix}"))
                            .small()
                            .label("Open")
                            .icon(IconName::LogIn)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_profile(ix, window, cx);
                            })),
                        cx,
                    )),
            );
            n += 1;
        }
        list.child(
            row(0).child(
                Button::new("list-new")
                    .small()
                    .ghost()
                    .label("New connection")
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_new_connection(window, cx);
                    })),
            ),
        )
    }

    fn render_home_recent(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let folded = self.home_view.recent_folded;
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(self.section_title(
                "home-recent-fold",
                "Recent queries",
                folded,
                theme,
                cx,
                |this| this.home_view.recent_folded = !this.home_view.recent_folded,
            ));
        if folded {
            return section.into_any_element();
        }
        if self.history.is_empty() {
            return section
                .child(caption_text(
                    "No recent queries yet. Run a query to see it here.".to_string(),
                    theme,
                ))
                .into_any_element();
        }
        for (ix, entry) in self.history.iter().take(8).enumerate() {
            let preview = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let sql = entry.sql.clone();
            section = section.child(
                div()
                    .id(format!("home-recent-{ix}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .h(px(TOOLBAR_HEIGHT))
                    .px(px(SPACE_2))
                    .rounded(px(RADIUS_MD))
                    .when(ix % 2 == 0, |row| row.bg(theme.table_even))
                    .hover(|this| this.bg(theme.secondary))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.new_tab_with_sql(sql.clone(), window, cx);
                    }))
                    .child(icon(IconName::Clock, 14., theme.muted_foreground))
                    .child(
                        div()
                            .text_size(px(FONT_LABEL))
                            .line_height(px(LEADING_LABEL))
                            .text_color(theme.foreground)
                            .truncate()
                            .child(preview),
                    ),
            );
        }
        section.into_any_element()
    }
}
