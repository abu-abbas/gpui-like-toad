use super::*;

#[derive(Clone)]
struct ProfileDrag {
    id: String,
    name: String,
}

impl Render for ProfileDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px(px(SPACE_2))
            .py(px(SPACE_1))
            .rounded(px(RADIUS_MD))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .child(self.name.clone())
    }
}

fn folder_color(name: &str, theme: &gpui_kit::component::Theme) -> Hsla {
    theme::ACCENTS
        .iter()
        .find(|(label, _, _)| *label == name)
        .map_or(theme.muted_foreground, |(_, light, dark)| {
            gpui_kit::rgb(if theme.is_dark() { *dark } else { *light }).into()
        })
}

pub(super) fn new_folder_item(view: &Entity<Workbench>, parent: &str) -> PopupMenuItem {
    let view = view.clone();
    let parent = parent.to_owned();
    PopupMenuItem::new("New folder…")
        .icon(Icon::new(IconName::Folder))
        .on_click(move |_, window, cx| {
            view.update(cx, |this, cx| {
                let field = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
                cx.subscribe_in(&field, window, |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.create_folder(cx);
                    }
                })
                .detach();
                this.rail = RailPanel::Connections;
                this.sidebar_compact = false;
                this.side_conn_folded = false;
                this.folder_draft = Some((parent.clone(), field.clone()));
                window.defer(cx, move |window, cx| {
                    field.update(cx, |s, cx| s.focus(window, cx))
                });
                cx.notify();
            });
        })
}

impl Workbench {
    fn create_folder(&mut self, cx: &mut Context<Self>) {
        let Some((parent, field)) = &self.folder_draft else {
            return;
        };
        let name = Self::input_text(field, cx);
        let name = name.trim();
        if name.is_empty() || name.contains('/') {
            self.conn.status = "Enter one folder name without a slash.".into();
            cx.notify();
            return;
        }
        let path = if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}/{name}")
        };
        // An empty color denotes a persistent folder with the default appearance.
        let mut folders = self.connection_folder_colors.clone();
        folders.entry(path.clone()).or_default();
        match store::save_connection_folder_colors(&folders) {
            Ok(()) => {
                self.connection_folder_colors = folders;
                let mut ancestor = path.as_str();
                while let Some((parent, _)) = ancestor.rsplit_once('/') {
                    self.collapsed_connection_folders.remove(parent);
                    ancestor = parent;
                }
                self.folder_draft = None;
            }
            Err(err) => self.push_message(format!("Could not save folder: {err}")),
        }
        cx.notify();
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
        let drop_highlight = theme.accent;
        let root_view = cx.entity();
        tree = tree.child(
            div()
                .id("connections-root-drop")
                .rounded(px(RADIUS_MD))
                .drag_over::<ProfileDrag>(move |style, _, _, _| style.bg(drop_highlight))
                .on_drop(cx.listener(|this, drag: &ProfileDrag, _, cx| {
                    this.move_profile_to_folder(&drag.id, "", cx);
                }))
                .context_menu(move |menu, _, _| menu.item(new_folder_item(&root_view, "")))
                .child(
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
                ),
        );
        if folded {
            return tree;
        }

        if let Some((parent, field)) = &self.folder_draft {
            tree = tree.child(
                div()
                    .flex()
                    .flex_col()
                    .child(caption_text(
                        if parent.is_empty() {
                            "New folder".into()
                        } else {
                            format!("New folder in {parent}")
                        },
                        theme,
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(Input::new(field).small()),
                            )
                            .child(
                                Button::new("folder-save")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Check)
                                    .on_click(cx.listener(|this, _, _, cx| this.create_folder(cx))),
                            )
                            .child(
                                Button::new("folder-cancel")
                                    .small()
                                    .ghost()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.folder_draft = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        let mut branches = div()
            .flex()
            .flex_col()
            .ml(px(12.))
            .pl(px(10.))
            .border_l_1()
            .border_color(theme.muted_foreground.opacity(0.30));
        let mut folders = std::collections::BTreeMap::<Vec<&str>, Vec<usize>>::new();
        for (folder, profile) in self
            .profiles
            .iter()
            .enumerate()
            .map(|(ix, p)| (p.folder.as_str(), Some(ix)))
            .chain(
                self.connection_folder_colors
                    .keys()
                    .map(|f| (f.as_str(), None)),
            )
        {
            let mut parts = Vec::new();
            for part in folder.split('/').filter(|part| !part.is_empty()) {
                parts.push(part);
                folders.entry(parts.clone()).or_default();
            }
            if let Some(ix) = profile {
                if parts.is_empty() {
                    branches = branches.child(self.render_profile_row(ix, keyboard, theme, cx));
                } else {
                    folders.entry(parts).or_default().push(ix);
                }
            }
        }
        let mut folded_path: Option<Vec<&str>> = None;
        for (parts, entries) in folders {
            if folded_path
                .as_ref()
                .is_some_and(|prefix| parts.starts_with(prefix))
            {
                continue;
            }
            let folder = parts.join("/");
            let name = folder.clone();
            let label = parts.last().unwrap().to_string();
            let indent = 12. * (parts.len() - 1) as f32;
            let collapsed = self.collapsed_connection_folders.contains(&name);
            let drop_folder = name.clone();
            let menu_folder = name.clone();
            let view = cx.entity();
            let color = folder_color(
                self.connection_folder_colors
                    .get(&folder)
                    .map_or("", String::as_str),
                theme,
            );
            branches = branches.child(
                div()
                    .id(format!("folder-drop-{folder}"))
                    .ml(px(indent))
                    .rounded(px(RADIUS_MD))
                    .drag_over::<ProfileDrag>(move |style, _, _, _| style.bg(drop_highlight))
                    .on_drop(cx.listener(move |this, drag: &ProfileDrag, _, cx| {
                        this.move_profile_to_folder(&drag.id, &drop_folder, cx);
                    }))
                    .context_menu(move |menu, window, cx| {
                        let folder = menu_folder.clone();
                        let view = view.clone();
                        menu.item(new_folder_item(&view, &folder)).submenu(
                            "Folder color",
                            window,
                            cx,
                            move |mut menu, _, cx| {
                                let current = view
                                    .read(cx)
                                    .connection_folder_colors
                                    .get(&folder)
                                    .map_or("", String::as_str);
                                for label in std::iter::once("Default")
                                    .chain(theme::ACCENTS.iter().map(|(label, _, _)| *label))
                                {
                                    let value = if label == "Default" { "" } else { label };
                                    let view = view.clone();
                                    let folder = folder.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(label)
                                            .icon(
                                                Icon::new(IconName::Folder)
                                                    .text_color(folder_color(value, cx.theme())),
                                            )
                                            .checked(current == value)
                                            .on_click(move |_, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.set_connection_folder_color(
                                                        &folder, value, cx,
                                                    )
                                                })
                                            }),
                                    );
                                }
                                menu
                            },
                        )
                    })
                    .child(
                        sidebar::tree_button(
                            Button::new(format!("connection-folder-{folder}")),
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
                        .child(tree_label(
                            if collapsed {
                                IconName::Folder
                            } else {
                                IconName::FolderOpen
                            },
                            label,
                            None,
                            color,
                            theme,
                        )),
                    ),
            );
            folded_path = collapsed.then_some(parts);
            if !collapsed {
                branches = branches.child(
                    div()
                        .flex()
                        .flex_col()
                        .ml(px(12. + indent))
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
        let environment = profile.access.environment;
        let environment_color = match environment {
            ConnectionEnvironment::Development => theme.success,
            ConnectionEnvironment::Staging => theme.warning,
            ConnectionEnvironment::Production => theme.danger,
            ConnectionEnvironment::None => theme.muted_foreground,
        };
        let focused = self.focused_profile_id.as_deref() == Some(profile.id.as_str());
        let profile_id = profile.id.clone();
        let parent_folder = profile.folder.clone();
        let view = cx.entity();
        div()
            .id(("profile-row", ix))
            .on_drag(
                ProfileDrag {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                },
                |drag, _, _, cx| {
                    let preview = drag.clone();
                    cx.new(|_| preview)
                },
            )
            .flex()
            .items_center()
            .flex_none()
            .rounded(px(RADIUS_MD))
            .context_menu(move |menu, _, cx| {
                let workbench = view.read(cx);
                let menu = menu.item(new_folder_item(&view, &parent_folder));
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
                        .when(active || focused, |row| {
                            row.font_weight(FontWeight::SEMIBOLD)
                        })
                        .when(environment != ConnectionEnvironment::None, |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_size(theme::ui_size(theme, FONT_CAPTION))
                                    .text_color(if connected {
                                        environment_color
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(environment.label()),
                            )
                        })
                        .when(
                            connected && environment == ConnectionEnvironment::None,
                            |row| row.child(status_dot(theme.success)),
                        ),
                    ),
            )
    }
}

fn connection_tree_label(
    symbol: IconName,
    text: String,
    count: Option<usize>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    tree_label(symbol, text, count, theme.muted_foreground, theme)
}

fn tree_label(
    symbol: IconName,
    text: String,
    count: Option<usize>,
    icon_color: Hsla,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .w_full()
        .min_w(px(0.))
        .gap(px(6.))
        .child(icon(symbol, 14., icon_color))
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
