use super::*;

impl Workbench {
    pub(super) fn render_connection_explorer(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(self.render_session_card(theme, cx))
            .child(
                div().px(px(SPACE_2)).flex_none().child(
                    explorer::control("explorer-new-sql", theme)
                        .h(px(explorer::EXPLORER_CONTROL_HEIGHT))
                        .w_full()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .w_full()
                                .child(icon(IconName::SquarePen, 15., theme.foreground))
                                .child("New SQL editor"),
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx))),
                ),
            )
            .child(self.render_explorer(theme, cx))
    }

    /// Connection name and session actions above the explorer.
    fn render_session_card(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sessions: Vec<_> = std::iter::once(&self.conn)
            .chain(&self.parked)
            .filter(|conn| conn.session.is_some())
            .map(|conn| (conn.id, conn.label()))
            .collect();
        let current = self.conn.id;
        let view = cx.entity();
        div().flex_none().px(px(8.)).py(px(6.)).child(
            Button::new("explorer-connection")
                .ghost()
                .w_full()
                .h(px(32.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap(px(8.))
                        .child(icon(IconName::Database, 16., theme.primary))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(self.conn.label()),
                        )
                        .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
                )
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.scrollable(true).max_h(px(320.)).min_w(px(200.));
                    for (id, label) in &sessions {
                        let id = *id;
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(id == current)
                                .on_click(move |_, _, cx: &mut App| {
                                    view.update(cx, |this, cx| {
                                        if id == this.conn.id
                                            || !this
                                                .conn_by_id(id)
                                                .is_some_and(|conn| conn.session.is_some())
                                        {
                                            return;
                                        }
                                        this.activate_conn(id, cx);
                                        this.focused_profile_id = this
                                            .conn
                                            .last_connection
                                            .as_ref()
                                            .and_then(|c| c.profile_ix)
                                            .and_then(|ix| this.profiles.get(ix))
                                            .map(|p| p.id.clone());
                                        if let Some(ix) =
                                            this.tabs.iter().rposition(|tab| tab.conn == Some(id))
                                        {
                                            this.activate_tab(ix, cx);
                                        } else {
                                            this.leave_whats_new();
                                            this.show_empty_workspace = true;
                                            cx.notify();
                                        }
                                    })
                                }),
                        );
                    }
                    menu
                }),
        )
    }

    pub(super) fn render_profile_list(
        &self,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let profiles = self.profiles.len();
        let folded = self.side_conn_folded;
        let mut tree = div()
            .id("connections-tree")
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_y_scroll();
        tree = tree.child(
            sidebar::tree_button(Button::new("saved-connections"), keyboard, cx)
                .small()
                .w_full()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.side_conn_folded = !this.side_conn_folded;
                    cx.notify();
                }))
                .child(connection_tree_label(
                    if folded {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    },
                    "My Connections".into(),
                    Some(profiles),
                    theme,
                )),
        );
        if folded {
            return tree;
        }

        let mut branches = div()
            .flex()
            .flex_col()
            .ml(px(12.))
            .pl(px(10.))
            .border_l_1()
            .border_color(theme.muted_foreground.opacity(0.30));
        let mut folders = std::collections::BTreeMap::<&str, Vec<usize>>::new();
        for (ix, profile) in self.profiles.iter().enumerate() {
            if profile.folder.is_empty() {
                branches = branches.child(self.render_profile_row(ix, keyboard, theme, cx));
            } else {
                folders.entry(&profile.folder).or_default().push(ix);
            }
        }
        for (folder, entries) in folders {
            let name = folder.to_owned();
            let collapsed = self.collapsed_connection_folders.contains(&name);
            branches = branches.child(
                sidebar::tree_button(
                    Button::new(format!("connection-folder-{name}")),
                    keyboard,
                    cx,
                )
                .small()
                .w_full()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed_connection_folders.remove(&name) {
                        this.collapsed_connection_folders.insert(name.clone());
                    }
                    cx.notify();
                }))
                .child(connection_tree_label(
                    if collapsed {
                        IconName::Folder
                    } else {
                        IconName::FolderOpen
                    },
                    folder.to_owned(),
                    None,
                    theme,
                )),
            );
            if !collapsed {
                branches = branches.child(
                    div()
                        .flex()
                        .flex_col()
                        .ml(px(12.))
                        .pl(px(10.))
                        .border_l_1()
                        .border_color(theme.muted_foreground.opacity(0.30))
                        .children(
                            entries
                                .into_iter()
                                .map(|ix| self.render_profile_row(ix, keyboard, theme, cx)),
                        ),
                );
            }
        }
        tree = tree.child(branches);
        if self.show_demo {
            let active = self.conn.session.is_some() && self.conn.demo_mode;
            tree = tree.child(
                sidebar::tree_button(Button::new("demo"), keyboard, cx)
                    .small()
                    .w_full()
                    .disabled(self.conn.busy)
                    .when(active, |button| button.text_color(theme.primary))
                    .on_click(cx.listener(|this, _, _, cx| this.connect(true, cx)))
                    .child(
                        connection_tree_label(IconName::Database, "Demo".into(), None, theme)
                            .when(active, |row| row.child(status_dot(theme.success))),
                    ),
            );
        }
        tree
    }

    fn render_profile_row(
        &self,
        ix: usize,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let profile = &self.profiles[ix];
        let active = self.conn.session.is_some()
            && self
                .conn
                .last_connection
                .as_ref()
                .and_then(|last| last.profile_ix)
                == Some(ix);
        let connected = self
            .find_open_conn(Some(ix), false)
            .and_then(|id| self.conn_by_id(id))
            .is_some_and(|c| c.session.is_some());
        let focused = self.focused_profile_id.as_deref() == Some(profile.id.as_str());
        let profile_id = profile.id.clone();
        let view = cx.entity();
        div()
            .id(("profile-row", ix))
            .flex()
            .items_center()
            .flex_none()
            .rounded(px(RADIUS_MD))
            .context_menu(move |menu, _, cx| {
                let workbench = view.read(cx);
                if let Some(id) = workbench.find_open_conn(Some(ix), false) {
                    let busy = workbench.conn_by_id(id).is_none_or(|conn| conn.busy);
                    let view = view.clone();
                    return menu.item(
                        PopupMenuItem::new("Disconnect")
                            .icon(Icon::new(IconName::LogOut))
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    if this
                                        .conn_by_id(id)
                                        .is_some_and(|conn| conn.is_open() && !conn.busy)
                                    {
                                        this.request_disconnect(id, window, cx);
                                    }
                                });
                            }),
                    );
                }
                let connect_view = view.clone();
                let edit_view = view.clone();
                let remove_view = view.clone();
                menu.item(
                    PopupMenuItem::new("Connect")
                        .icon(Icon::new(IconName::LogIn))
                        .on_click(move |_, window, cx| {
                            connect_view.update(cx, |this, cx| this.open_profile(ix, window, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new("Edit connection")
                        .icon(Icon::new(IconName::SquarePen))
                        .on_click(move |_, window, cx| {
                            edit_view.update(cx, |this, cx| {
                                if this.find_open_conn(Some(ix), false).is_none() {
                                    this.edit_profile(ix, window, cx);
                                }
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new("Remove connection")
                        .icon(Icon::new(IconName::Trash))
                        .on_click(move |_, _, cx| {
                            remove_view.update(cx, |this, cx| this.remove_profile(ix, cx));
                        }),
                )
            })
            .child(
                sidebar::tree_button(Button::new(("profile", ix)), keyboard, cx)
                    .small()
                    .w_full()
                    .tooltip(profile.name.clone())
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        let Some(ix) = this
                            .profiles
                            .iter()
                            .position(|profile| profile.id == profile_id)
                        else {
                            return;
                        };
                        this.focused_profile_id = Some(profile_id.clone());
                        if matches!(event, ClickEvent::Mouse(mouse) if mouse.down.click_count == 2)
                        {
                            this.open_profile(ix, window, cx);
                        } else {
                            cx.notify();
                        }
                    }))
                    .child(
                        connection_tree_label(
                            IconName::Database,
                            profile.name.clone(),
                            None,
                            theme,
                        )
                        .when(active || focused, |row| row.text_color(theme.primary))
                        .when(connected, |row| row.child(status_dot(theme.success))),
                    ),
            )
    }

    /// The New/Edit connection dialog, drawn as a modal over the whole
    /// window (scrim + centered card).
    pub(super) fn render_connection_dialog(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let field = |label: &str, input: Input| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(px(SPACE_1))
                .child(form_label(label, theme))
                .child(input.small().w_full())
        };
        let form = &self.connection_form;
        let mode_button = |id: &'static str, label: &'static str, tns: bool| {
            let active = form.use_tns == tns;
            Button::new(id)
                .small()
                .ghost()
                .label(label)
                .flex_1()
                .min_w(px(0.))
                .h(px(28.))
                .when(active, |button| {
                    button
                        .bg(theme.background)
                        .text_color(theme.foreground)
                        .border_1()
                        .border_color(theme.border)
                })
                .when(!active, |button| button.text_color(theme.muted_foreground))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.connection_form.use_tns = tns;
                    cx.notify();
                }))
        };
        let title = if form.editing_profile.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        let row = || div().flex().items_start().gap(px(SPACE_3));
        let mut body = div()
            .id("connection-dialog-body")
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(SPACE_3))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .child(
                row()
                    .child(field("Connection name", Input::new(&form.name_input)))
                    .child(
                        div()
                            .flex()
                            .w(relative(0.36))
                            .flex_none()
                            .child(field("Folder", Input::new(&form.folder_input))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(RADIUS_MD))
                    .bg(theme.secondary)
                    .child(mode_button("conn-mode-basic", "Basic", false))
                    .child(mode_button("conn-mode-tns", "TNS", true)),
            );
        if form.use_tns {
            body = body.child(field(
                "TNS alias / connect descriptor",
                Input::new(&form.tns_input),
            ));
        } else {
            body = body.child(
                row()
                    .child(field("Host", Input::new(&form.host_input)))
                    .child(
                        div()
                            .flex()
                            .w(px(72.))
                            .flex_none()
                            .child(field("Port", Input::new(&form.port_input))),
                    )
                    .child(
                        div()
                            .flex()
                            .w(relative(0.31))
                            .flex_none()
                            .child(field("Service name", Input::new(&form.service_input))),
                    ),
            );
        }
        body = body
            .child(
                row()
                    .child(field("Username", Input::new(&form.user_input)))
                    .child(field(
                        "Password",
                        Input::new(&form.password_input).mask_toggle(),
                    )),
            )
            .child(
                Checkbox::new("save-password")
                    .small()
                    .label("Save password")
                    .checked(form.save_password)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.connection_form.save_password = *checked;
                        cx.notify();
                    })),
            );
        let notice = self
            .conn
            .connect_error
            .clone()
            .map(|e| (true, e))
            .or_else(|| self.connection_form.form_notice.clone());
        if let Some((is_error, text)) = notice {
            body = body.child(
                div()
                    .p(px(SPACE_2))
                    .rounded(px(RADIUS_SM))
                    .text_size(crate::theme::ui_size(&theme, FONT_CAPTION))
                    .line_height(px(LEADING_CAPTION))
                    .text_color(if is_error {
                        theme.danger
                    } else {
                        theme.success
                    })
                    .border_1()
                    .border_color(if is_error {
                        theme.danger
                    } else {
                        theme.success
                    })
                    .child(text),
            );
        } else {
            let has_saved = self
                .connection_form
                .editing_profile
                .and_then(|ix| self.profiles.get(ix))
                .is_some_and(|p| p.save_password);
            let hint = if self.connection_form.save_password && has_saved {
                "Password tersimpan di keychain OS. Kosongkan untuk tetap pakai yang lama."
            } else if self.connection_form.save_password {
                "Password disimpan di keychain OS, bukan di file profil."
            } else {
                "Password tidak disimpan; diisi setiap connect."
            };
            body = body.child(caption_text(hint.to_string(), theme));
        }
        let action = |id| Button::new(id).small().h(px(32.)).flex_none();
        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .border_t_1()
            .border_color(theme.border)
            .child(
                action("test-connect")
                    .outline()
                    .min_w(px(72.))
                    .label("Test")
                    .disabled(self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
            )
            .child(
                action("save-profile")
                    .outline()
                    .min_w(px(72.))
                    .label("Save")
                    .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
            );
        if let Some(ix) = self
            .connection_form
            .editing_profile
            .filter(|&ix| self.find_open_conn(Some(ix), false).is_none())
        {
            footer = footer.child(
                action("remove-profile")
                    .w(px(32.))
                    .tooltip("Remove connection")
                    .icon(IconName::Trash)
                    .danger()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_profile(ix, cx);
                        this.close_connection_dialog(cx);
                    })),
            );
        }
        footer = footer
            .child(div().flex_1())
            .child(
                action("cancel-connect")
                    .ghost()
                    .min_w(px(72.))
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.close_connection_dialog(cx))),
            )
            .child(
                action("connect")
                    .min_w(px(114.))
                    .label(
                        if self.conn.busy
                            && matches!(self.conn.pending_op, Some(PendingOp::Connect))
                        {
                            "Connecting…"
                        } else {
                            "Connect"
                        },
                    )
                    .primary()
                    .disabled(self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.connect(false, cx))),
            );
        let card = div()
            .id("connection-dialog")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(520.))
            .max_h(relative(1.))
            .overflow_hidden()
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_4))
                    .h(px(44.))
                    .flex_none()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(IconName::Database, 18., theme.primary))
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::theme::ui_size(&theme, FONT_BODY))
                            .line_height(px(LEADING_BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("connection-dialog-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(28.))
                            .rounded(px(RADIUS_MD))
                            .hover(|this| this.bg(theme.secondary))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.close_connection_dialog(cx)),
                            )
                            .child(icon(IconName::X, 16., theme.muted_foreground)),
                    ),
            )
            .child(body)
            .child(footer);
        div()
            .id("connection-dialog-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(SPACE_4))
            .bg(gpui_kit::hsla(0., 0., 0., 0.45))
            // Swallow clicks so nothing under the modal reacts.
            .occlude()
            .child(card)
    }
}

fn connection_tree_label(
    symbol: IconName,
    text: String,
    count: Option<usize>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .w_full()
        .min_w(px(0.))
        .gap(px(6.))
        .child(icon(symbol, 14., theme.muted_foreground))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_left()
                .truncate()
                .child(text),
        )
        .children(count.map(|count| caption_text(count.to_string(), theme)))
}
