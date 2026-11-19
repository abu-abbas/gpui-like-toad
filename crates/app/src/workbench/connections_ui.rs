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
                        .children(ui_helpers::environment_badge(self.conn.access(), theme))
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

    /// The New/Edit connection dialog, drawn as a modal over the whole
    /// window (scrim + centered card).
    pub(super) fn render_connection_dialog(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let field = |label: &str| {
            div()
                .flex()
                .flex_col()
                .min_w(px(0.))
                .gap(px(SPACE_1))
                .child(form_label(label, theme))
        };
        let input_field = |label: &str, input: Input| {
            field(label).flex_1().child(
                input
                    .small()
                    .w_full()
                    .border_color(theme.input.opacity(0.45)),
            )
        };
        let form = &self.connection_form;
        let mut tabs = div()
            .flex()
            .items_center()
            .gap(px(SPACE_3))
            .px(px(SPACE_4))
            .flex_none();
        for (id, label, advanced) in [
            ("connection-general-tab", "General", false),
            ("connection-advanced-tab", "Advanced", true),
        ] {
            let selected = form.show_advanced == advanced;
            tabs = tabs.child(
                Button::new(id)
                    .small()
                    .ghost()
                    .h(px(30.))
                    .rounded_none()
                    .border_b_2()
                    .border_color(if selected {
                        theme.primary
                    } else {
                        theme.transparent
                    })
                    .text_color(if selected {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.connection_form.show_advanced = advanced;
                        cx.notify();
                    })),
            );
        }
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
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(SPACE_3))
            .px(px(SPACE_4))
            .py(px(SPACE_3));

        if !form.show_advanced {
            let use_tns = form.use_tns;
            let view = cx.entity();
            let connection_type = Button::new("connection-type")
                .small()
                .ghost()
                .bg(theme.secondary)
                .h(px(24.))
                .w_full()
                .child(select_trigger(
                    if use_tns { "TNS" } else { "Basic" }.into(),
                    theme,
                ))
                .dropdown_menu(move |menu, _, _| {
                    [("Basic", false), ("TNS", true)].into_iter().fold(
                        menu,
                        |menu, (label, tns)| {
                            let view = view.clone();
                            menu.item(PopupMenuItem::new(label).checked(use_tns == tns).on_click(
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.connection_form.use_tns = tns;
                                        cx.notify();
                                    })
                                },
                            ))
                        },
                    )
                });
            body = body.child(
                row()
                    .child(input_field("Connection name", Input::new(&form.name_input)))
                    .child(
                        field("Connection type")
                            .w(px(130.))
                            .flex_none()
                            .child(connection_type),
                    ),
            );
            if form.use_tns {
                body = body.child(
                    input_field(
                        "TNS alias / connect descriptor",
                        Input::new(&form.tns_input),
                    )
                    .flex_none(),
                );
            } else {
                body = body.child(
                    row()
                        .child(input_field("Host", Input::new(&form.host_input)))
                        .child(
                            div()
                                .flex()
                                .w(px(72.))
                                .flex_none()
                                .child(input_field("Port", Input::new(&form.port_input))),
                        )
                        .child(
                            div()
                                .flex()
                                .w(relative(0.31))
                                .flex_none()
                                .child(input_field(
                                    "Service name",
                                    Input::new(&form.service_input),
                                )),
                        ),
                );
            }
            body = body
                .child(
                    row()
                        .child(input_field("Username", Input::new(&form.user_input)))
                        .child(input_field(
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
        } else {
            body = body.child(
                div()
                    .w(relative(0.48))
                    .flex()
                    .child(input_field("Folder", Input::new(&form.folder_input))),
            );
            let access = form.access;
            let environment = Button::new("connection-environment")
                .small()
                .ghost()
                .bg(theme.secondary)
                .w_full()
                .child(select_trigger(access.environment.label().into(), theme));
            let view = cx.entity();
            let environment = environment.dropdown_menu(move |menu, _, _| {
                ConnectionEnvironment::ALL
                    .into_iter()
                    .fold(menu, |menu, option| {
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::new(option.label())
                                .checked(option == access.environment)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.connection_form.access.environment = option;
                                        if option == ConnectionEnvironment::Production {
                                            this.connection_form.access.protect_production = true;
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                    })
            });
            let access_controls = div()
                .flex()
                .flex_col()
                .gap(px(SPACE_2))
                .pt(px(SPACE_2))
                .child(form_label("Access and protection", theme))
                .child(field("Environment").w(px(172.)).child(environment))
                .child(
                    Checkbox::new("read-only")
                        .small()
                        .label("Read Only")
                        .checked(access.read_only)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.connection_form.access.read_only = *checked;
                            if *checked {
                                this.connection_form.access.sysdba = false;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("production-protection")
                        .small()
                        .label("Confirm production operations")
                        .checked(access.protect_production)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.connection_form.access.protect_production = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("connect-sysdba")
                        .small()
                        .label("Connect as SYSDBA")
                        .checked(access.sysdba)
                        .disabled(access.read_only)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.connection_form.access.sysdba = *checked;
                            cx.notify();
                        })),
                )
                .when(access.read_only, |section| {
                    section.child(caption_text(
                        "Use an Oracle account with read-only grants for database-enforced permissions.".into(),
                        theme,
                    ))
                });
            body = body.child(access_controls);
        }
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
                    .line_height(crate::theme::ui_size(&theme, LEADING_CAPTION))
                    .text_color(if is_error {
                        theme.danger
                    } else {
                        theme.success
                    })
                    .child(text),
            );
        } else if !form.show_advanced {
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
            .child(
                action("test-connect")
                    .secondary()
                    .min_w(px(72.))
                    .label("Test")
                    .disabled(self.conn.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
            )
            .child(
                action("save-profile")
                    .secondary()
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
            .max_w(px(548.))
            .h(px(406.))
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
                    .child(icon(IconName::Database, 18., theme.primary))
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::theme::ui_size(&theme, FONT_BODY))
                            .line_height(crate::theme::ui_size(&theme, LEADING_BODY))
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
            .child(tabs)
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
            .bg(crate::theme::modal_scrim(theme))
            // Swallow clicks so nothing under the modal reacts.
            .occlude()
            .child(card)
    }
}
