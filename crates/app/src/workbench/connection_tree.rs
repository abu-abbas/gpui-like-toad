use super::*;

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};

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

#[derive(Clone)]
struct FolderDrag {
    path: String,
}

impl Render for FolderDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px(px(SPACE_2))
            .py(px(SPACE_1))
            .rounded(px(RADIUS_MD))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .child(
                self.path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&self.path)
                    .to_owned(),
            )
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

/// The "delete folder?" dialog: what the folder holds.
pub(super) struct FolderDelete {
    path: String,
    connections: usize,
    subfolders: usize,
    open: usize,
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
    fn start_rename_folder(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
        field.update(cx, |s, cx| s.set_value(name, window, cx));
        cx.subscribe_in(&field, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.rename_folder(cx);
            }
        })
        .detach();
        self.folder_draft = None;
        self.folder_rename = Some((path.to_owned(), field.clone()));
        window.defer(cx, move |window, cx| {
            field.update(cx, |s, cx| s.focus(window, cx))
        });
        cx.notify();
    }

    fn all_folder_paths(&self) -> std::collections::BTreeSet<String> {
        let mut all = std::collections::BTreeSet::new();
        for folder in self
            .profiles
            .iter()
            .map(|p| p.folder.as_str())
            .chain(self.connection_folder_colors.keys().map(String::as_str))
        {
            let mut path = String::new();
            for part in folder.split('/').filter(|p| !p.is_empty()) {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                all.insert(path.clone());
            }
        }
        all
    }

    /// Re-root every folder path at or below `from` (see
    /// `store::folder_rebase`) in profiles, colors and fold state, and
    /// save. `drop_from` removes the folder entry itself (delete case).
    fn rebase_folders(&mut self, from: &str, to: &str, drop_from: bool) -> std::io::Result<()> {
        let mut profiles = self.profiles.clone();
        for p in &mut profiles {
            if let Some(f) = store::folder_rebase(&p.folder, from, to) {
                p.folder = f;
            }
        }
        let mut colors = std::collections::BTreeMap::new();
        for (path, color) in &self.connection_folder_colors {
            if drop_from && path == from {
                continue;
            }
            let path = store::folder_rebase(path, from, to).unwrap_or_else(|| path.clone());
            colors.entry(path).or_insert_with(|| color.clone());
        }
        store::save_profiles(&profiles)?;
        store::save_connection_folder_colors(&colors)?;
        self.profiles = profiles;
        self.connection_folder_colors = colors;
        self.collapsed_connection_folders = self
            .collapsed_connection_folders
            .iter()
            .filter(|f| !(drop_from && f.as_str() == from))
            .map(|f| store::folder_rebase(f, from, to).unwrap_or_else(|| f.clone()))
            .collect();
        Ok(())
    }

    fn rename_folder(&mut self, cx: &mut Context<Self>) {
        let Some((path, field)) = &self.folder_rename else {
            return;
        };
        let path = path.clone();
        let name = Self::input_text(field, cx);
        let name = name.trim();
        if name.is_empty() || name.contains('/') {
            self.conn.status = "Enter one folder name without a slash.".into();
            cx.notify();
            return;
        }
        let new_path = match path.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{name}"),
            None => name.to_owned(),
        };
        if new_path != path {
            if self.all_folder_paths().contains(&new_path) {
                self.conn.status = format!("Folder '{name}' already exists here.");
                cx.notify();
                return;
            }
            // Keep an open connection's folder in sync too: it is only a label.
            if let Err(err) = self.rebase_folders(&path, &new_path, false) {
                self.push_message(format!("Could not rename folder: {err}"));
                cx.notify();
                return;
            }
        }
        self.folder_rename = None;
        cx.notify();
    }

    fn request_delete_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        let inside: Vec<usize> = (0..self.profiles.len())
            .filter(|&ix| store::folder_in(&self.profiles[ix].folder, path))
            .collect();
        let open = inside
            .iter()
            .filter(|&&ix| self.find_open_conn(Some(ix), false).is_some())
            .count();
        let subfolders = self
            .all_folder_paths()
            .iter()
            .filter(|f| f.as_str() != path && store::folder_in(f, path))
            .count();
        self.folder_delete = Some(FolderDelete {
            path: path.to_owned(),
            connections: inside.len(),
            subfolders,
            open,
        });
        cx.notify();
    }

    /// Delete the folder; `with_connections` also removes the connections
    /// (and subfolders) inside it, otherwise they move up one level.
    fn confirm_delete_folder(&mut self, with_connections: bool, cx: &mut Context<Self>) {
        let Some(dialog) = self.folder_delete.take() else {
            return;
        };
        let path = dialog.path;
        if with_connections {
            let mut ids = Vec::new();
            for ix in (0..self.profiles.len()).rev() {
                if store::folder_in(&self.profiles[ix].folder, &path) {
                    ids.push(ix);
                }
            }
            for ix in ids {
                self.remove_profile(ix, cx);
            }
            let colors: std::collections::BTreeMap<_, _> = self
                .connection_folder_colors
                .iter()
                .filter(|(f, _)| !store::folder_in(f, &path))
                .map(|(f, c)| (f.clone(), c.clone()))
                .collect();
            match store::save_connection_folder_colors(&colors) {
                Ok(()) => self.connection_folder_colors = colors,
                Err(err) => self.push_message(format!("Could not save folders: {err}")),
            }
            self.collapsed_connection_folders
                .retain(|f| !store::folder_in(f, &path));
        } else {
            let parent = path.rsplit_once('/').map_or("", |(p, _)| p).to_owned();
            if let Err(err) = self.rebase_folders(&path, &parent, true) {
                self.push_message(format!("Could not delete folder: {err}"));
            }
        }
        cx.notify();
    }

    pub(super) fn render_folder_delete(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(dialog) = self.folder_delete.as_ref() else {
            return div().into_any_element();
        };
        let name = dialog.path.rsplit('/').next().unwrap_or(&dialog.path);
        let text = if dialog.connections == 0 && dialog.subfolders == 0 {
            "This folder is empty.".to_owned()
        } else {
            format!(
                "This folder contains {} connection(s) and {} subfolder(s). Move its contents up one level or delete everything.{}",
                dialog.connections,
                dialog.subfolders,
                if dialog.open > 0 {
                    format!(
                        " Disconnect the {} open connection(s) before deleting them.",
                        dialog.open
                    )
                } else {
                    String::new()
                }
            )
        };
        let empty = dialog.connections == 0 && dialog.subfolders == 0;
        let card = div()
            .id("folder-delete")
            .flex()
            .flex_col()
            .w(px(440.))
            .rounded(px(RADIUS_MD))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.folder_delete = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                            .line_height(crate::theme::ui_size(theme, LEADING_HEADING_MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("Delete folder {name}?")),
                    )
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(theme, FONT_BODY))
                            .line_height(crate::theme::ui_size(theme, LEADING_BODY))
                            .text_color(theme.muted_foreground)
                            .child(text),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(SPACE_2))
                    .p(px(SPACE_4))
                    .bg(theme.muted)
                    .child(
                        Button::new("folder-delete-cancel")
                            .flex_1()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.folder_delete = None;
                                cx.notify();
                            })),
                    )
                    .when(!empty, |row| {
                        row.child(
                            Button::new("folder-delete-all")
                                .flex_1()
                                .label("Delete All")
                                .danger()
                                .disabled(dialog.open > 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete_folder(true, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("folder-delete-move")
                            .flex_1()
                            .label(if empty { "Delete" } else { "Move Contents Up" })
                            .primary()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.confirm_delete_folder(false, cx)),
                            ),
                    ),
            );
        div()
            .id("folder-delete-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(160.))
            .bg(crate::theme::modal_scrim(theme))
            .occlude()
            .child(card)
            .into_any_element()
    }

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

    /// Move folder `from` (with everything in it) under `target`
    /// (`""` = top level). Refuses its own subtree and name clashes.
    fn move_folder(&mut self, from: &str, target: &str, cx: &mut Context<Self>) {
        let name = from.rsplit('/').next().unwrap_or(from);
        let parent = from.rsplit_once('/').map_or("", |(p, _)| p);
        if target == parent {
            return;
        }
        if store::folder_in(target, from) {
            self.conn.status = "A folder cannot go inside itself.".into();
            cx.notify();
            return;
        }
        let new_path = if target.is_empty() {
            name.to_owned()
        } else {
            format!("{target}/{name}")
        };
        if self.all_folder_paths().contains(&new_path) {
            self.conn.status = format!("'{name}' already exists in that folder.");
            cx.notify();
            return;
        }
        match self.rebase_folders(from, &new_path, false) {
            Ok(()) => {
                self.collapsed_connection_folders.remove(target);
                self.side_conn_folded = false;
            }
            Err(err) => self.push_message(format!("Could not move folder: {err}")),
        }
        cx.notify();
    }

    /// The connections tree inside a scroll area in both directions: deep
    /// nesting or long names can outgrow the column, then a horizontal
    /// scrollbar stays visible under the tree (macOS hides idle scrollbars,
    /// so the default fade-out mode looked like there was none at all).
    pub(super) fn render_profile_list(
        &self,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let handle = &self.connections_scroll;
        div()
            .relative()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .child(
                div()
                    .id("connections-scroll")
                    .track_scroll(handle)
                    .flex()
                    .flex_col()
                    .min_h(px(0.))
                    .overflow_scroll()
                    // Room for the horizontal scrollbar under the last row.
                    .pb(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .min_w(px(self.profile_tree_min_width()))
                            .child(self.render_profile_tree(keyboard, theme, cx)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        Scrollbar::horizontal(handle)
                            .id("connections-scrollbar-x")
                            .mode(ScrollbarMode::Always)
                            .viewport_from_layout(),
                    )
                    .child(
                        Scrollbar::vertical(handle)
                            .id("connections-scrollbar-y")
                            .viewport_from_layout(),
                    ),
            )
    }

    /// Rough width the widest tree row needs: indentation per folder level
    /// plus its label. Rows truncate inside the column, so without this the
    /// content would never be wider than the column and nothing would scroll.
    fn profile_tree_min_width(&self) -> f32 {
        const LEVEL: f32 = 20.;
        const CHAR: f32 = 7.4;
        let row = |depth: usize, chars: usize, extra: f32| {
            22. + LEVEL * depth as f32 + 20. + CHAR * chars as f32 + extra
        };
        let depth_of = |folder: &str| folder.split('/').filter(|p| !p.is_empty()).count();
        let profiles = self.profiles.iter().map(|p| {
            // Room for the environment tag (DEV, PROD...) when it shows.
            let tag = if p.access.environment == ConnectionEnvironment::None {
                16.
            } else {
                52.
            };
            row(depth_of(&p.folder), p.name.chars().count(), tag)
        });
        let folders = self.all_folder_paths().into_iter().map(|f| {
            let depth = depth_of(&f);
            let label = f.rsplit('/').next().unwrap_or("").chars().count();
            row(depth.saturating_sub(1), label, 8.)
        });
        profiles.chain(folders).fold(0., f32::max)
    }

    fn render_profile_tree(
        &self,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let profiles = self.profiles.len();
        let folded = self.side_conn_folded;
        let mut tree = div().id("connections-tree").flex().flex_col().min_h(px(0.));
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
                .drag_over::<FolderDrag>(move |style, _, _, _| style.bg(drop_highlight))
                .on_drop(cx.listener(|this, drag: &FolderDrag, _, cx| {
                    this.move_folder(&drag.path, "", cx);
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
        let mut branches = div().flex().flex_col().ml(px(12.)).pl(px(10.));
        let mut root_profiles = Vec::new();
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
                    root_profiles.push(ix);
                } else {
                    folders.entry(parts).or_default().push(ix);
                }
            }
        }
        let mut headers = std::collections::BTreeMap::<Vec<&str>, FolderNode>::new();
        for (parts, entries) in folders {
            let folder = parts.join("/");
            let name = folder.clone();
            let label = parts.last().unwrap().to_string();
            let collapsed = self.collapsed_connection_folders.contains(&name);
            let drop_folder = name.clone();
            let drop_into = name.clone();
            let drag_path = name.clone();
            let menu_folder = name.clone();
            let view = cx.entity();
            let color = folder_color(
                self.connection_folder_colors
                    .get(&folder)
                    .map_or("", String::as_str),
                theme,
            );
            let header = div()
                .id(format!("folder-drop-{folder}"))
                .rounded(px(RADIUS_MD))
                .drag_over::<ProfileDrag>(move |style, _, _, _| style.bg(drop_highlight))
                .on_drop(cx.listener(move |this, drag: &ProfileDrag, _, cx| {
                    this.move_profile_to_folder(&drag.id, &drop_folder, cx);
                }))
                .on_drag(FolderDrag { path: drag_path }, |drag, _, _, cx| {
                    let preview = drag.clone();
                    cx.new(|_| preview)
                })
                .drag_over::<FolderDrag>(move |style, _, _, _| style.bg(drop_highlight))
                .on_drop(cx.listener(move |this, drag: &FolderDrag, _, cx| {
                    this.move_folder(&drag.path, &drop_into, cx);
                }))
                .context_menu(move |menu, window, cx| {
                    let folder = menu_folder.clone();
                    let view = view.clone();
                    let rename_view = view.clone();
                    let rename_path = folder.clone();
                    let delete_view = view.clone();
                    let delete_path = folder.clone();
                    menu.item(new_folder_item(&view, &folder))
                        .item(
                            PopupMenuItem::new("Rename folder…")
                                .icon(Icon::new(IconName::SquarePen))
                                .on_click(move |_, window, cx| {
                                    rename_view.update(cx, |this, cx| {
                                        this.start_rename_folder(&rename_path, window, cx)
                                    })
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Delete folder…")
                                .icon(Icon::new(IconName::Trash))
                                .on_click(move |_, _, cx| {
                                    delete_view.update(cx, |this, cx| {
                                        this.request_delete_folder(&delete_path, cx)
                                    })
                                }),
                        )
                        .submenu("Folder color", window, cx, move |mut menu, _, cx| {
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
                                                this.set_connection_folder_color(&folder, value, cx)
                                            })
                                        }),
                                );
                            }
                            menu
                        })
                })
                .child(match &self.folder_rename {
                    Some((path, field)) if *path == folder => {
                        folder_rename_row(field, color, cx).into_any_element()
                    }
                    _ => sidebar::tree_button(
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
                        Icon::new(if collapsed {
                            IconName::Folder
                        } else {
                            IconName::FolderOpen
                        })
                        .size(px(14.))
                        .text_color(color),
                        label,
                        None,
                        theme,
                    ))
                    .into_any_element(),
                })
                .into_any_element();
            headers.insert(
                parts,
                FolderNode {
                    header,
                    entries,
                    collapsed,
                },
            );
        }
        let root_nodes = self.render_branch(&[], root_profiles, &mut headers, keyboard, theme, cx);
        let root_last = root_nodes.len().saturating_sub(1);
        for (i, node) in root_nodes.into_iter().enumerate() {
            branches = branches.child(guide_child(node, i == root_last, theme));
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
        // Greyed out while the extension for its engine is not installed.
        let extension = profile.kind.extension_id();
        let available = self.extension_ready(extension);
        let extension_name = store::extension_info(extension).map_or("the", |e| e.name);
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
                    let transfer_view = view.clone();
                    let menu = menu.item(
                        PopupMenuItem::new("Data Transfer…")
                            .icon(Icon::new(IconName::ArrowLeftRight))
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                transfer_view.update(cx, |this, cx| {
                                    this.activate_conn(id, cx);
                                    this.open_data_transfer(None, window, cx);
                                });
                            }),
                    );
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
                // Not connected: shown but disabled, so the entry is always
                // discoverable; it needs an open connection as its source.
                menu.item(
                    PopupMenuItem::new("Data Transfer… (connect first)")
                        .icon(Icon::new(IconName::ArrowLeftRight))
                        .disabled(true),
                )
                .item(
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
                    .tooltip(if available {
                        profile.name.clone()
                    } else {
                        format!(
                            "{}: install the {extension_name} extension in Settings > Extensions to open it.",
                            profile.name
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        let Some(ix) = this
                            .profiles
                            .iter()
                            .position(|profile| profile.id == profile_id)
                        else {
                            return;
                        };
                        this.focused_profile_id = Some(profile_id.clone());
                        this.leave_tools();
                        if matches!(event, ClickEvent::Mouse(mouse) if mouse.down.click_count == 2)
                        {
                            this.open_profile(ix, window, cx);
                        } else {
                            cx.notify();
                        }
                    }))
                    .child(
                        tree_label(
                            db_icon::icon(profile.kind, connected, theme),
                            profile.name.clone(),
                            None,
                            theme,
                        )
                        .gap(px(8.))
                        .when(!available, |row| {
                            row.text_color(theme.muted_foreground).child(
                                div()
                                    .flex_none()
                                    .text_size(theme::ui_size(theme, FONT_CAPTION))
                                    .child(format!("Install {extension_name}")),
                            )
                        })
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
    symbol: impl Into<Icon>,
    text: String,
    count: Option<usize>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    tree_label(
        Icon::new(symbol)
            .size(px(14.))
            .text_color(theme.muted_foreground),
        text,
        count,
        theme,
    )
}

fn tree_label(
    symbol: Icon,
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
        .child(symbol)
        .child(
            div()
                // With a count the text pushes it to the right edge; without
                // one a trailing flag (DEV, the connected dot) sits right
                // after the name so it never scrolls out of sight.
                .when(count.is_some(), |text| text.flex_1())
                .min_w(px(0.))
                .text_left()
                .truncate()
                .child(text),
        )
        .children(count.map(|count| caption_text(count.to_string(), theme)))
}

/// The folder row while it is being renamed: the same folder icon with an
/// input in place of the label, so the edit happens where the folder is.
fn folder_rename_row(
    field: &Entity<InputState>,
    color: Hsla,
    cx: &mut Context<Workbench>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(28.))
        .pl(px(8.))
        .child(icon(IconName::FolderOpen, 14., color))
        .child(
            div()
                .flex_1()
                .min_w(px(80.))
                .child(Input::new(field).xsmall()),
        )
        .child(
            Button::new("folder-rename-save")
                .xsmall()
                .ghost()
                .icon(IconName::Check)
                .tooltip("Save")
                .on_click(cx.listener(|this, _, _, cx| this.rename_folder(cx))),
        )
        .child(
            Button::new("folder-rename-cancel")
                .xsmall()
                .ghost()
                .icon(IconName::X)
                .tooltip("Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.folder_rename = None;
                    cx.notify();
                })),
        )
}

struct FolderNode {
    header: gpui_kit::AnyElement,
    entries: Vec<usize>,
    collapsed: bool,
}

/// One child of a tree node with its guide: a vertical line on the left
/// that stops at the elbow for the last child, plus the short horizontal stub.
fn guide_child(
    el: gpui_kit::AnyElement,
    last: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let color = theme.muted_foreground.opacity(0.30);
    let mut line = div()
        .absolute()
        .left(px(-10.))
        .top(px(0.))
        .w(px(1.))
        .bg(color);
    line = if last {
        line.h(px(15.))
    } else {
        line.bottom(px(0.))
    };
    div()
        .relative()
        .child(line)
        .child(
            div()
                .absolute()
                .left(px(-10.))
                .top(px(15.))
                .w(px(8.))
                .h(px(1.))
                .bg(color),
        )
        .child(el)
        .into_any_element()
}

impl Workbench {
    /// Children of the folder `prefix` (its profiles, then subfolders),
    /// each wrapped with its guide line. Collapsed folders show no children.
    fn render_branch<'a>(
        &self,
        prefix: &[&'a str],
        profiles: Vec<usize>,
        headers: &mut std::collections::BTreeMap<Vec<&'a str>, FolderNode>,
        keyboard: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let mut nodes: Vec<gpui_kit::AnyElement> = profiles
            .into_iter()
            .map(|ix| {
                self.render_profile_row(ix, keyboard, theme, cx)
                    .into_any_element()
            })
            .collect();
        let subfolders: Vec<Vec<&'a str>> = headers
            .keys()
            .filter(|key| key.len() == prefix.len() + 1 && key.starts_with(prefix))
            .cloned()
            .collect();
        for key in subfolders {
            let Some(node) = headers.remove(&key) else {
                continue;
            };
            let children = if node.collapsed {
                Vec::new()
            } else {
                self.render_branch(&key, node.entries, headers, keyboard, theme, cx)
            };
            let count = children.len();
            let last = count.saturating_sub(1);
            nodes.push(
                div()
                    .flex()
                    .flex_col()
                    .child(node.header)
                    .when(count > 0, |folder| {
                        folder.child(
                            div().flex().flex_col().ml(px(12.)).pl(px(10.)).children(
                                children
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, el)| guide_child(el, i == last, theme)),
                            ),
                        )
                    })
                    .into_any_element(),
            );
        }
        nodes
    }
}
