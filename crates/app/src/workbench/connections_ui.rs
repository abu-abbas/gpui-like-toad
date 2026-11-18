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
                div().px(px(SPACE_2)).pt(px(SPACE_1)).flex_none().child(
                    Button::new("explorer-new-sql")
                        .small()
                        .w_full()
                        .icon(IconName::SquarePen)
                        .label("New SQL editor")
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
        let Some(last) = self.conn.last_connection.clone() else {
            return div()
                .px(px(SPACE_3))
                .pt(px(SPACE_3))
                .child(caption_text(
                    "Not connected. Pick a connection or try Demo.".to_string(),
                    theme,
                ))
                .into_any_element();
        };
        let connected = self.conn.session.is_some();
        let connecting = self.conn.busy && matches!(self.conn.pending_op, Some(PendingOp::Connect));
        let dot = if connecting {
            theme.warning
        } else if connected {
            theme.success
        } else {
            theme.danger
        };
        let view = cx.entity();
        let busy = self.conn.busy;
        let demo = last.demo;
        let menu_button = Button::new("connection-actions")
            .icon(IconName::EllipsisVertical)
            .ghost()
            .small()
            .dropdown_menu(move |menu, _, _| {
                let refresh_view = view.clone();
                let toggle_view = view.clone();
                let edit_view = view.clone();
                menu.min_w(px(200.))
                    .item(
                        PopupMenuItem::new("Refresh")
                            .icon(Icon::new(IconName::RefreshCw))
                            .disabled(!connected || busy)
                            .on_click(move |_, _, cx: &mut App| {
                                refresh_view.update(cx, |this, cx| this.refresh_schema(cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new(if connected { "Disconnect" } else { "Connect" })
                            .icon(Icon::new(if connected {
                                IconName::LogOut
                            } else {
                                IconName::LogIn
                            }))
                            .disabled(busy)
                            .on_click(move |_, window, cx: &mut App| {
                                toggle_view.update(cx, |this, cx| {
                                    if this.conn.session.is_some() {
                                        let id = this.conn.id;
                                        this.request_disconnect(id, window, cx);
                                    } else {
                                        this.reconnect_last(window, cx);
                                    }
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Edit connection")
                            .icon(Icon::new(IconName::SquarePen))
                            // Only while disconnected: editing the profile a
                            // live session uses would not change that session.
                            // Demo has no host/user to edit.
                            .disabled(connected || busy || demo)
                            .on_click(move |_, window, cx: &mut App| {
                                edit_view
                                    .update(cx, |this, cx| this.edit_last_connection(window, cx));
                            }),
                    )
            });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .flex_none()
            .px(px(SPACE_2))
            .h(px(TOOLBAR_HEIGHT))
            .child(status_dot(dot))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                    .line_height(px(LEADING_LABEL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(last.profile.name.clone()),
            )
            .child(menu_button)
            .into_any_element()
    }

    pub(super) fn render_profile_list(
        &self,
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
            Button::new("saved-connections")
                .small()
                .ghost()
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

        let mut folders = std::collections::BTreeMap::<&str, Vec<usize>>::new();
        for (ix, profile) in self.profiles.iter().enumerate() {
            if profile.folder.is_empty() {
                tree = tree.child(self.render_profile_row(ix, false, theme, cx));
            } else {
                folders.entry(&profile.folder).or_default().push(ix);
            }
        }
        for (folder, entries) in folders {
            let name = folder.to_owned();
            let collapsed = self.collapsed_connection_folders.contains(&name);
            tree = tree.child(
                Button::new(format!("connection-folder-{name}"))
                    .small()
                    .ghost()
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
                for ix in entries {
                    tree = tree.child(self.render_profile_row(ix, true, theme, cx));
                }
            }
        }
        if self.show_demo {
            let active = self.conn.session.is_some() && self.conn.demo_mode;
            tree = tree.child(
                Button::new("demo")
                    .small()
                    .ghost()
                    .w_full()
                    .disabled(self.conn.busy)
                    .when(active, |button| button.bg(theme.accent))
                    .on_click(cx.listener(|this, _, window, cx| this.connect(true, window, cx)))
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
        in_folder: bool,
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
        let view = cx.entity();
        let edit_view = view.clone();
        div()
            .id(("profile-row", ix))
            .flex()
            .items_center()
            .flex_none()
            .pl(px(if in_folder { 14. } else { 4. }))
            .rounded(px(RADIUS_MD))
            .when(active, |row| row.bg(theme.accent))
            .context_menu(move |menu, _, _| {
                let edit_view = edit_view.clone();
                let view = view.clone();
                menu.item(
                    PopupMenuItem::new("Edit connection")
                        .icon(Icon::new(IconName::SquarePen))
                        .on_click(move |_, window, cx| {
                            edit_view.update(cx, |this, cx| this.edit_profile(ix, window, cx));
                        }),
                )
                .item(
                    PopupMenuItem::new("Remove connection")
                        .icon(Icon::new(IconName::Trash))
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.remove_profile(ix, cx));
                        }),
                )
            })
            .child(
                Button::new(("profile", ix))
                    .small()
                    .ghost()
                    .w_full()
                    .tooltip(profile.name.clone())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_profile(ix, window, cx)),
                    )
                    .child(
                        connection_tree_label(
                            IconName::Database,
                            profile.name.clone(),
                            None,
                            theme,
                        )
                        .when(active, |row| {
                            row.text_color(theme.accent_foreground)
                                .child(status_dot(theme.success))
                        }),
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
        let field = |label: &str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_1))
                .child(form_label(label, theme))
                .child(Input::new(input).w_full())
        };
        let mode_button = |id: &'static str, label: &'static str, tns: bool| {
            let active = self.connection_form.use_tns == tns;
            let mut b = div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .flex_1()
                .h(px(28.))
                .rounded(px(RADIUS_SM))
                .text_size(crate::theme::ui_size(&theme, FONT_LABEL))
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.connection_form.use_tns = tns;
                    cx.notify();
                }))
                .child(label);
            if active {
                b = b
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.foreground);
            } else {
                b = b.text_color(theme.muted_foreground);
            }
            b
        };
        let title = if self.connection_form.editing_profile.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .p(px(SPACE_4))
            .child(
                div()
                    .flex()
                    .gap(px(SPACE_2))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(field("Connection name", &self.connection_form.name_input)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(field("Folder", &self.connection_form.folder_input)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(RADIUS_MD))
                    .bg(theme.secondary)
                    .child(mode_button("conn-mode-basic", "Basic", false))
                    .child(mode_button("conn-mode-tns", "TNS", true)),
            );
        if self.connection_form.use_tns {
            body = body.child(field(
                "TNS alias / connect descriptor",
                &self.connection_form.tns_input,
            ));
        } else {
            body = body
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(SPACE_2))
                        .child(
                            div()
                                .flex_1()
                                .child(field("Host", &self.connection_form.host_input)),
                        )
                        .child(
                            div()
                                .w(px(96.))
                                .child(field("Port", &self.connection_form.port_input)),
                        ),
                )
                .child(field("Service name", &self.connection_form.service_input));
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .gap(px(SPACE_2))
                .child(
                    div()
                        .flex_1()
                        .child(field("Username", &self.connection_form.user_input)),
                )
                .child(
                    div().flex_1().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_1))
                            .child(form_label("Password", theme))
                            .child(
                                Input::new(&self.connection_form.password_input)
                                    .mask_toggle()
                                    .w_full(),
                            ),
                    ),
                ),
        );
        body = body.child(
            Checkbox::new("save-password")
                .label("Save password")
                .checked(self.connection_form.save_password)
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
        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .px(px(SPACE_4))
            .py(px(SPACE_3))
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("test-connect")
                    .label("Test")
                    .disabled(self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
            )
            .child(
                Button::new("save-profile")
                    .label("Save")
                    .icon(IconName::Save)
                    .on_click(cx.listener(|this, _, _, cx| this.save_profile(cx))),
            );
        if let Some(ix) = self.connection_form.editing_profile {
            footer = footer.child(
                Button::new("remove-profile")
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
                Button::new("cancel-connect")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.close_connection_dialog(cx))),
            )
            .child(
                Button::new("connect")
                    .label(
                        if self.conn.busy
                            && matches!(self.conn.pending_op, Some(PendingOp::Connect))
                        {
                            "Connecting…"
                        } else {
                            "Connect"
                        },
                    )
                    .icon(IconName::Database)
                    .primary()
                    .disabled(self.conn.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.connect(false, window, cx))),
            );
        let card = div()
            .id("connection-dialog")
            .flex()
            .flex_col()
            .w(px(480.))
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
                    .h(px(48.))
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
