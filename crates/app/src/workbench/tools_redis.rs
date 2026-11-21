//! Redis browser: database and type dropdowns, a SCAN pattern and the key
//! list in the explorer column; the key's value and a console in the work
//! area.

use devtools::redis_tool::{
    self, display, ttl_label, ttl_short, KeyDetail, KeyEntry, KeyValue, RedisSession, ServerInfo,
};
use ora_core::store::ToolInstance;
use std::sync::{Arc, Mutex};

use super::tools::{
    connect_redis, env_tag, resolve_password, spawn_tool_job, tool_icon, with_session,
    ConfirmAction, SharedRedis, ToolConfirm,
};
use super::*;

/// Key types for the type dropdown: SCAN TYPE name and label.
const KEY_TYPES: [(&str, &str); 6] = [
    ("string", "String"),
    ("hash", "Hash"),
    ("list", "List"),
    ("set", "Set"),
    ("zset", "Sorted set"),
    ("stream", "Stream"),
];

/// Longest string value drawn as text; the rest is cut (Copy has all).
const STRING_SHOWN: usize = 200 * 1024;
/// Longest element of a hash / list / set drawn in its row.
const ITEM_SHOWN: usize = 2_000;

fn type_label(kind: &str) -> &str {
    KEY_TYPES
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(kind, |(_, l)| l)
}

fn type_icon(kind: &str) -> IconName {
    match kind {
        "string" => IconName::CaseSensitive,
        "hash" => IconName::Hash,
        "list" => IconName::ListOrdered,
        "set" => IconName::Braces,
        "zset" => IconName::ChartColumn,
        "stream" => IconName::Activity,
        _ => IconName::Key,
    }
}

fn clip(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RedisTab {
    Value,
    Console,
}

pub(super) struct ConsoleLine {
    pub db: u32,
    pub command: String,
    pub output: String,
    pub error: bool,
}

pub(super) struct RedisView {
    pub session: Option<SharedRedis>,
    pub connecting: bool,
    pub error: Option<String>,
    pub info: Option<ServerInfo>,
    pub keyspace: Vec<(u32, u64)>,
    pub db: u32,
    pub kind: Option<&'static str>,
    pub pattern: Entity<InputState>,
    pub keys: Vec<KeyEntry>,
    /// SCAN cursor to continue from; 0 once the scan is complete.
    pub cursor: u64,
    pub scanning: bool,
    pub selected: Option<Vec<u8>>,
    pub detail: Option<KeyDetail>,
    pub detail_loading: bool,
    pub detail_error: Option<String>,
    pub tab: RedisTab,
    pub console_input: Entity<InputState>,
    pub console_log: Vec<ConsoleLine>,
    pub console_busy: bool,
    /// Bumped when the list is reset, so an older scan page is dropped.
    pub epoch: u64,
    pub list_scroll: UniformListScrollHandle,
    pub console_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl RedisView {
    pub fn new(tool: &ToolInstance, window: &mut Window, cx: &mut Context<Workbench>) -> Self {
        let pattern =
            cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Pattern, e.g. session:*"));
        let console_input = cx.new(|cx| {
            InputState::new(&mut *window, cx).placeholder("Command, e.g. HGETALL user:1")
        });
        let id = tool.id.clone();
        let id2 = tool.id.clone();
        let subs = vec![
            cx.subscribe_in(
                &pattern,
                window,
                move |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.redis_rescan(&id, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &console_input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.redis_run_console(&id2, window, cx);
                    }
                },
            ),
        ];
        RedisView {
            session: None,
            connecting: false,
            error: None,
            info: None,
            keyspace: Vec::new(),
            db: tool.database,
            kind: None,
            pattern,
            keys: Vec::new(),
            cursor: 0,
            scanning: false,
            selected: None,
            detail: None,
            detail_loading: false,
            detail_error: None,
            tab: RedisTab::Value,
            console_input,
            console_log: Vec::new(),
            console_busy: false,
            epoch: 0,
            list_scroll: UniformListScrollHandle::new(),
            console_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    fn key_count(&self, db: u32) -> u64 {
        self.keyspace
            .iter()
            .find(|(d, _)| *d == db)
            .map_or(0, |(_, n)| *n)
    }
}

impl Workbench {
    fn redis_view(&mut self, id: &str) -> Option<&mut RedisView> {
        self.tools.redis.get_mut(id)
    }

    pub(super) fn redis_connect(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(tool) = self.tools.get(id).cloned() else {
            return;
        };
        let typed = self.tools.session_passwords.get(id).cloned();
        let Some(view) = self.redis_view(id) else {
            return;
        };
        view.connecting = true;
        view.error = None;
        view.epoch += 1;
        let epoch = view.epoch;
        let db = view.db;
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || {
                let password = resolve_password(&tool, typed);
                let (mut session, info) = connect_redis(&tool, password)?;
                let keyspace = session.keyspace().unwrap_or_default();
                let page = session.scan(db, "*", None, 0)?;
                Ok::<_, String>((session, info, keyspace, page))
            },
            move |this, result, _| {
                let Some(view) = this.tools.redis.get_mut(&id) else {
                    return;
                };
                if view.epoch != epoch {
                    return;
                }
                view.connecting = false;
                match result {
                    Ok((session, info, keyspace, page)) => {
                        view.session = Some(Arc::new(Mutex::new(session)));
                        view.info = Some(info);
                        view.keyspace = keyspace;
                        view.keys = page.keys;
                        view.cursor = page.cursor;
                    }
                    Err(err) => view.error = Some(err),
                }
            },
        );
        cx.notify();
    }

    /// Start the key list over with the current db, type and pattern.
    pub(super) fn redis_rescan(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(view) = self.redis_view(id) else {
            return;
        };
        view.epoch += 1;
        view.keys.clear();
        view.cursor = 0;
        self.redis_scan_page(id, cx);
    }

    pub(super) fn redis_scan_page(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(view) = self.tools.redis.get_mut(id) else {
            return;
        };
        let Some(session) = view.session.clone() else {
            return;
        };
        view.scanning = true;
        let epoch = view.epoch;
        let db = view.db;
        let kind = view.kind;
        let cursor = view.cursor;
        let pattern = view.pattern.read(cx).value().trim().to_string();
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || {
                with_session(&session, |s| {
                    let page = s.scan(db, &pattern, kind, cursor)?;
                    let keyspace = s.keyspace().ok();
                    Ok::<_, String>((page, keyspace))
                })
            },
            move |this, result, _| {
                let Some(view) = this.tools.redis.get_mut(&id) else {
                    return;
                };
                if view.epoch != epoch {
                    return;
                }
                view.scanning = false;
                match result {
                    Ok((page, keyspace)) => {
                        let mut seen: std::collections::HashSet<Vec<u8>> =
                            view.keys.iter().map(|k| k.key.clone()).collect();
                        view.keys
                            .extend(page.keys.into_iter().filter(|k| seen.insert(k.key.clone())));
                        view.keys
                            .sort_by(|a, b| redis_tool::natural_cmp(&a.key, &b.key));
                        view.cursor = page.cursor;
                        if let Some(ks) = keyspace {
                            view.keyspace = ks;
                        }
                        view.error = None;
                    }
                    Err(err) => view.error = Some(err),
                }
            },
        );
        cx.notify();
    }

    fn redis_select_db(&mut self, id: &str, db: u32, cx: &mut Context<Self>) {
        if let Some(view) = self.redis_view(id) {
            view.db = db;
            view.selected = None;
            view.detail = None;
            view.detail_error = None;
        }
        self.redis_rescan(id, cx);
    }

    fn redis_set_kind(&mut self, id: &str, kind: Option<&'static str>, cx: &mut Context<Self>) {
        if let Some(view) = self.redis_view(id) {
            view.kind = kind;
        }
        self.redis_rescan(id, cx);
    }

    pub(super) fn redis_select_key(&mut self, id: &str, key: Vec<u8>, cx: &mut Context<Self>) {
        let Some(view) = self.tools.redis.get_mut(id) else {
            return;
        };
        let Some(session) = view.session.clone() else {
            return;
        };
        view.selected = Some(key.clone());
        view.detail_loading = true;
        view.detail_error = None;
        view.tab = RedisTab::Value;
        let db = view.db;
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || with_session(&session, |s| s.load(db, &key)),
            move |this, result, _| {
                let Some(view) = this.tools.redis.get_mut(&id) else {
                    return;
                };
                // A later click won.
                let current = view.selected.as_ref();
                let matches = match &result {
                    Ok(d) => current == Some(&d.key),
                    Err(_) => true,
                };
                if !matches {
                    return;
                }
                view.detail_loading = false;
                match result {
                    Ok(detail) => view.detail = Some(detail),
                    Err(err) => {
                        view.detail = None;
                        view.detail_error = Some(err);
                    }
                }
            },
        );
        cx.notify();
    }

    fn redis_refresh(&mut self, id: &str, cx: &mut Context<Self>) {
        let selected = self.redis_view(id).and_then(|v| v.selected.clone());
        self.redis_rescan(id, cx);
        if let Some(key) = selected {
            self.redis_select_key(id, key, cx);
        }
    }

    fn redis_reconnect(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.redis_view(id) {
            view.session = None;
            view.keys.clear();
            view.cursor = 0;
        }
        self.redis_connect(id, cx);
    }

    pub(super) fn redis_run_console(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tool) = self.tools.get(id).cloned() else {
            return;
        };
        let Some(view) = self.tools.redis.get_mut(id) else {
            return;
        };
        let Some(session) = view.session.clone() else {
            return;
        };
        if view.console_busy {
            return;
        }
        let line = view.console_input.read(cx).value().trim().to_string();
        if line.is_empty() {
            return;
        }
        let input = view.console_input.clone();
        input.update(cx, |state, cx| state.set_value("", window, cx));
        if line.eq_ignore_ascii_case("clear") {
            view.console_log.clear();
            cx.notify();
            return;
        }
        view.console_busy = true;
        let db = view.db;
        let id = id.to_string();
        let command = line.clone();
        spawn_tool_job(
            cx,
            move || {
                with_session(&session, |s| {
                    s.console(db, &line, tool.allow_dangerous, tool.read_only())
                })
            },
            move |this, result, _| {
                let Some(view) = this.tools.redis.get_mut(&id) else {
                    return;
                };
                view.console_busy = false;
                let (output, error) = match result {
                    Ok(out) => (out, false),
                    Err(err) => (err, true),
                };
                view.console_log.push(ConsoleLine {
                    db,
                    command,
                    output,
                    error,
                });
                if view.console_log.len() > 500 {
                    view.console_log.remove(0);
                }
                view.console_scroll.scroll_to_bottom();
            },
        );
        cx.notify();
    }

    fn redis_request_delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(view) = self.tools.redis.get(id) else {
            return;
        };
        let Some(key) = view.selected.clone() else {
            return;
        };
        self.tools.confirm = Some(ToolConfirm {
            title: format!("Delete key {}?", clip(display(&key), 80)),
            text: format!(
                "The key is removed from db{} on the server. This cannot be undone.",
                view.db
            ),
            button: "Delete Key",
            action: ConfirmAction::DeleteKey {
                tool: id.to_string(),
                db: view.db,
                key,
            },
        });
        cx.notify();
    }

    pub(super) fn redis_delete_key(
        &mut self,
        id: &str,
        db: u32,
        key: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        if self.tools.get(id).is_none_or(ToolInstance::read_only) {
            return;
        }
        let Some(session) = self.tools.redis.get(id).and_then(|v| v.session.clone()) else {
            return;
        };
        let id = id.to_string();
        spawn_tool_job(
            cx,
            {
                let key = key.clone();
                move || with_session(&session, |s| s.delete_key(db, &key))
            },
            move |this, result, cx| {
                let Some(view) = this.tools.redis.get_mut(&id) else {
                    return;
                };
                match result {
                    Ok(_) => {
                        view.keys.retain(|k| k.key != key);
                        if view.selected.as_ref() == Some(&key) {
                            view.selected = None;
                            view.detail = None;
                        }
                        if let Some(entry) = view.keyspace.iter_mut().find(|(d, _)| *d == db) {
                            entry.1 = entry.1.saturating_sub(1);
                        }
                    }
                    Err(err) => view.detail_error = Some(err),
                }
                cx.notify();
            },
        );
    }

    // -----------------------------------------------------------------
    // Explorer column
    // -----------------------------------------------------------------

    pub(super) fn render_redis_explorer(
        &self,
        tool: &ToolInstance,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = self.tools.redis.get(&tool.id);
        let entity = cx.entity();
        let id = tool.id.clone();
        let live = view.is_some_and(|v| v.session.is_some());
        let header = {
            let (e1, e2, e3, e4) = (
                entity.clone(),
                entity.clone(),
                entity.clone(),
                entity.clone(),
            );
            let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
            let kind = tool.kind;
            div().flex_none().px(px(8.)).py(px(6.)).child(
                Button::new("tool-session")
                    .ghost()
                    .w_full()
                    .h(px(32.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .w_full()
                            .gap(px(8.))
                            .child(tool_icon(tool.kind, live, theme))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_left()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tool.name.clone()),
                            )
                            .children(env_tag(tool.environment, theme))
                            .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
                    )
                    .dropdown_menu(move |menu, _, _| {
                        let (e1, e2, e3, e4) = (e1.clone(), e2.clone(), e3.clone(), e4.clone());
                        let (i1, i2, i3, i4) = (i1.clone(), i2.clone(), i3.clone(), i4.clone());
                        menu.min_w(px(200.))
                            .item(
                                PopupMenuItem::new("Refresh")
                                    .icon(Icon::new(IconName::RefreshCw))
                                    .on_click(move |_, _, cx| {
                                        e1.update(cx, |this, cx| this.redis_refresh(&i1, cx))
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Reconnect")
                                    .icon(Icon::new(IconName::Plug))
                                    .on_click(move |_, _, cx| {
                                        e2.update(cx, |this, cx| this.redis_reconnect(&i2, cx))
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Edit connection…")
                                    .icon(Icon::new(IconName::SquarePen))
                                    .on_click(move |_, window, cx| {
                                        e3.update(cx, |this, cx| {
                                            this.open_tool_form(kind, Some(i3.clone()), window, cx)
                                        })
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new("Disconnect")
                                    .icon(Icon::new(IconName::LogOut))
                                    .on_click(move |_, _, cx| {
                                        e4.update(cx, |this, cx| this.close_tool(&i4, cx))
                                    }),
                            )
                    }),
            )
        };
        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(header);
        let Some(view) = view else {
            return col;
        };
        if view.session.is_none() {
            return col.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(SPACE_3))
                    .p(px(SPACE_4))
                    .child(if view.connecting {
                        loading_state(&format!("Connecting to {}…", tool.address()), theme)
                    } else {
                        empty_state_text(
                            view.error
                                .clone()
                                .unwrap_or_else(|| "Not connected.".into()),
                            theme,
                        )
                        .text_center()
                    })
                    .when(!view.connecting, |c| {
                        let id = id.clone();
                        c.child(
                            Button::new("redis-retry")
                                .small()
                                .label("Try again")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.redis_reconnect(&id, cx)
                                    }),
                                ),
                        )
                    }),
            );
        }
        let databases = view.info.as_ref().map_or(16, |i| i.databases).clamp(1, 256);
        let counts: Vec<(u32, u64)> = (0..databases).map(|d| (d, view.key_count(d))).collect();
        let current_db = view.db;
        let db_button = {
            let entity = entity.clone();
            let id = id.clone();
            explorer::control("redis-db", theme)
                .w_full()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap(px(6.))
                        .child(icon(IconName::Database, 14., theme.muted_foreground))
                        .child(div().flex_1().text_left().child(format!("db{current_db}")))
                        .child(caption_text(
                            format!("{} keys", view.key_count(current_db)),
                            theme,
                        ))
                        .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
                )
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(200.));
                    for (db, n) in &counts {
                        let (db, n) = (*db, *n);
                        let entity = entity.clone();
                        let id = id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(if n > 0 {
                                format!("db{db}  ·  {n} keys")
                            } else {
                                format!("db{db}")
                            })
                            .checked(db == current_db)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.redis_select_db(&id, db, cx))
                            }),
                        );
                    }
                    menu
                })
        };
        let current_kind = view.kind;
        let kind_button = {
            let entity = entity.clone();
            let id = id.clone();
            explorer::control("redis-kind", theme)
                .w_full()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap(px(6.))
                        .child(icon(
                            current_kind.map_or(IconName::Layers, type_icon),
                            14.,
                            theme.muted_foreground,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .text_left()
                                .child(current_kind.map_or("All types", type_label).to_string()),
                        )
                        .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
                )
                .dropdown_menu(move |menu, _, _| {
                    let entity2 = entity.clone();
                    let id2 = id.clone();
                    let mut menu = menu.min_w(px(200.)).item(
                        PopupMenuItem::new("All types")
                            .checked(current_kind.is_none())
                            .on_click(move |_, _, cx| {
                                entity2.update(cx, |this, cx| this.redis_set_kind(&id2, None, cx))
                            }),
                    );
                    for (kind, label) in KEY_TYPES {
                        let entity = entity.clone();
                        let id = id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .icon(Icon::new(type_icon(kind)))
                                .checked(current_kind == Some(kind))
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.redis_set_kind(&id, Some(kind), cx)
                                    })
                                }),
                        );
                    }
                    menu
                })
        };
        let search = Input::new(&view.pattern)
            .small()
            .prefix(icon(IconName::Search, 14., theme.muted_foreground))
            .w_full();
        let controls = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_1))
            .px(px(SPACE_2))
            .pb(px(SPACE_2))
            .child(db_button)
            .child(kind_button)
            .child(search);
        col = col.child(controls);

        let list = if view.keys.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p(px(SPACE_4))
                .child(if view.scanning {
                    loading_state("Scanning keys…", theme)
                } else {
                    empty_state_text(
                        view.error
                            .clone()
                            .unwrap_or_else(|| "No keys match.".into()),
                        theme,
                    )
                    .text_center()
                })
                .into_any_element()
        } else {
            let keys: Vec<KeyEntry> = view.keys.clone();
            let count = keys.len();
            let selected = view.selected.clone();
            let theme2 = theme.clone();
            let id = id.clone();
            div()
                .flex_1()
                .min_h(px(0.))
                .px(px(SPACE_2))
                .child(
                    uniform_list(
                        "redis-keys",
                        count,
                        cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .map(|ix| {
                                    let entry = &keys[ix];
                                    let is_selected = selected.as_ref() == Some(&entry.key);
                                    render_key_row(ix, entry, is_selected, &id, &theme2, cx)
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&view.list_scroll)
                    .size_full(),
                )
                .into_any_element()
        };
        col = col.child(list);
        let more = view.cursor != 0;
        let footer_text = if view.scanning && !view.keys.is_empty() {
            "Scanning…".to_string()
        } else if more {
            format!("{} keys loaded, more on the server", view.keys.len())
        } else {
            format!("{} keys", view.keys.len())
        };
        let id_more = id.clone();
        col.child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(SPACE_2))
                .h(px(32.))
                .px(px(SPACE_3))
                .border_t_1()
                .border_color(theme.border)
                .child(caption_text(footer_text, theme).flex_1())
                .when(more, |row| {
                    row.child(
                        Button::new("redis-more")
                            .xsmall()
                            .ghost()
                            .label("Load more")
                            .disabled(view.scanning)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.redis_scan_page(&id_more, cx)
                            })),
                    )
                }),
        )
    }

    // -----------------------------------------------------------------
    // Work area
    // -----------------------------------------------------------------

    pub(super) fn render_redis_main(
        &self,
        tool: &ToolInstance,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = tool.id.clone();
        let Some(view) = self.tools.redis.get(&tool.id) else {
            return div().size_full().into_any_element();
        };
        let tab_button = |tab: RedisTab, label: &'static str, cx: &mut Context<Self>| {
            let selected = view.tab == tab;
            let id = id.clone();
            Button::new(label)
                .small()
                .ghost()
                .h(px(SUB_TAB_HEIGHT))
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
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(v) = this.tools.redis.get_mut(&id) {
                        v.tab = tab;
                        if tab == RedisTab::Console {
                            let input = v.console_input.clone();
                            input.update(cx, |state, cx| state.focus(window, cx));
                        }
                    }
                    cx.notify();
                }))
        };
        let read_only = tool.read_only();
        let has_key = view.detail.is_some();
        let id_refresh = id.clone();
        let id_delete = id.clone();
        let tabs =
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(SPACE_3))
                .px(px(SPACE_4))
                .h(px(TOOLBAR_HEIGHT))
                .border_b_1()
                .border_color(theme.border)
                .child(tab_button(RedisTab::Value, "Value", cx))
                .child(tab_button(RedisTab::Console, "Console", cx))
                .child(div().flex_1())
                .children(view.info.as_ref().map(|info| {
                    caption_text(
                        format!(
                            "Redis {} · {} · db{}",
                            info.version,
                            tool.address(),
                            view.db
                        ),
                        theme,
                    )
                }))
                .child(
                    Button::new("redis-refresh")
                        .small()
                        .ghost()
                        .icon(IconName::RefreshCw)
                        .tooltip("Refresh keys and value")
                        .disabled(view.session.is_none())
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.redis_refresh(&id_refresh, cx)),
                        ),
                )
                .child(
                    Button::new("redis-delete")
                        .small()
                        .ghost()
                        .icon(IconName::Trash)
                        .tooltip(if read_only {
                            "PROD is read only"
                        } else {
                            "Delete key"
                        })
                        .disabled(!has_key || read_only || view.tab != RedisTab::Value)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.redis_request_delete(&id_delete, cx)
                        })),
                );
        let body = match view.tab {
            RedisTab::Value => self.render_redis_value(view, theme, cx).into_any_element(),
            RedisTab::Console => self
                .render_redis_console(tool, view, theme, cx)
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(theme.background)
            .child(tabs)
            .child(body)
            .into_any_element()
    }

    fn render_redis_value(
        &self,
        view: &RedisView,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let center = |child: gpui_kit::AnyElement| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(SPACE_4))
                .child(child)
        };
        if view.detail_loading && view.detail.is_none() {
            return center(loading_state("Loading value…", theme).into_any_element());
        }
        if let Some(err) = &view.detail_error {
            return center(
                body_text(err.clone(), theme)
                    .text_color(theme.danger)
                    .into_any_element(),
            );
        }
        let Some(detail) = &view.detail else {
            return center(
                empty_state_text("Select a key to see its value.".into(), theme).into_any_element(),
            );
        };
        let mut meta = vec![
            type_label(&detail.kind).to_string(),
            format!("TTL {}", ttl_label(detail.ttl_ms)),
        ];
        if detail.kind == "string" {
            meta.push(format!("{} bytes", detail.len));
        } else {
            meta.push(format!("{} items", detail.len));
        }
        if let Some(enc) = &detail.encoding {
            meta.push(enc.clone());
        }
        if let Some(mem) = detail.memory {
            meta.push(format!("{} in memory", human_bytes(mem)));
        }
        let copy_text = value_text(&detail.value);
        let header = div()
            .flex()
            .items_start()
            .flex_none()
            .gap(px(SPACE_3))
            .px(px(SPACE_4))
            .pt(px(SPACE_4))
            .pb(px(SPACE_3))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(SPACE_1))
                    .child(
                        div()
                            .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                            .line_height(crate::theme::ui_size(theme, LEADING_HEADING_MD))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(clip(display(&detail.key), 400)),
                    )
                    .child(caption_text(meta.join("  ·  "), theme)),
            )
            .child(
                Button::new("redis-copy")
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .label("Copy value")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                            copy_text.clone(),
                        ))
                    }),
            );
        let mono = theme.mono_font_family.clone();
        let body: gpui_kit::AnyElement = match &detail.value {
            KeyValue::Str { bytes, truncated } => {
                let text = display(bytes);
                let (text, json) = match redis_tool::pretty_json(&text) {
                    Some(pretty) => (pretty, true),
                    None => (text, false),
                };
                let cut = text.len() > STRING_SHOWN;
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(
                        div()
                            .p(px(SPACE_3))
                            .rounded(px(RADIUS_MD))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary.opacity(0.4))
                            .font_family(mono)
                            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                            .whitespace_normal()
                            .child(clip(text, STRING_SHOWN)),
                    )
                    .when(json, |c| {
                        c.child(caption_text("JSON, shown indented.".into(), theme))
                    })
                    .when(*truncated || cut, |c| {
                        c.child(caption_text(
                            "Only the first part of this value is shown.".into(),
                            theme,
                        ))
                    })
                    .into_any_element()
            }
            KeyValue::Hash(pairs) => grid(
                ("Field", "Value"),
                pairs.iter().map(|(f, v)| (display(f), display(v))),
                detail.len,
                theme,
            ),
            KeyValue::List(items) => grid(
                ("Index", "Value"),
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), display(v))),
                detail.len,
                theme,
            ),
            KeyValue::Set(items) => grid(
                ("#", "Member"),
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| ((i + 1).to_string(), display(v))),
                detail.len,
                theme,
            ),
            KeyValue::ZSet(items) => grid(
                ("Score", "Member"),
                items.iter().map(|(m, s)| (format_score(*s), display(m))),
                detail.len,
                theme,
            ),
            KeyValue::Raw(text) => div()
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .font_family(mono)
                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                .whitespace_normal()
                .child(clip(text.clone(), STRING_SHOWN))
                .into_any_element(),
            KeyValue::Missing => {
                empty_state_text("The key no longer exists.".into(), theme).into_any_element()
            }
        };
        let _ = cx;
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(header)
            .child(
                div()
                    .id("redis-value-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(SPACE_4))
                    .pb(px(SPACE_4))
                    .child(body),
            )
    }

    fn render_redis_console(
        &self,
        tool: &ToolInstance,
        view: &RedisView,
        theme: &gpui_kit::component::Theme,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mono = theme.mono_font_family.clone();
        let mut log = div()
            .id("redis-console-log")
            .track_scroll(&view.console_scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .gap(px(SPACE_2))
            .p(px(SPACE_4))
            .font_family(mono.clone())
            .text_size(crate::theme::ui_size(theme, FONT_LABEL));
        if view.console_log.is_empty() {
            let rule = if tool.read_only() {
                "PROD is read only: only read commands run."
            } else if tool.allow_dangerous {
                "Dangerous commands are allowed on this instance."
            } else {
                "FLUSHDB, FLUSHALL, KEYS, CONFIG and the like are blocked. Allow them in the connection settings."
            };
            log = log.child(caption_text(
                format!(
                    "Commands run on db{}. Switch databases with the dropdown. {rule} Type clear to empty the log.",
                    view.db
                ),
                theme,
            ));
        }
        for line in &view.console_log {
            log = log.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_color(theme.primary)
                            .child(format!("db{}> {}", line.db, line.command)),
                    )
                    .child(
                        div()
                            .whitespace_normal()
                            .text_color(if line.error {
                                theme.danger
                            } else {
                                theme.foreground
                            })
                            .child(clip(line.output.clone(), STRING_SHOWN)),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(log)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_2))
                    .px(px(SPACE_4))
                    .py(px(SPACE_2))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono)
                            .text_color(theme.primary)
                            .child(format!("db{}>", view.db)),
                    )
                    .child(
                        Input::new(&view.console_input)
                            .small()
                            .appearance(false)
                            .w_full()
                            .disabled(view.session.is_none()),
                    )
                    .when(view.console_busy, |row| {
                        row.child(gpui_kit::component::spinner::Spinner::new().small())
                    }),
            )
    }
}

fn render_key_row(
    ix: usize,
    entry: &KeyEntry,
    selected: bool,
    tool_id: &str,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<Workbench>,
) -> impl IntoElement {
    let key = entry.key.clone();
    let tool_id = tool_id.to_string();
    let mut row = div()
        .id(("redis-key", ix))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE_2))
        .h(px(TREE_ROW_HEIGHT))
        .px(px(SPACE_2))
        .rounded(px(RADIUS_SM))
        .text_size(crate::theme::ui_size(theme, FONT_LABEL))
        .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
        .on_click(
            cx.listener(move |this, _, _, cx| this.redis_select_key(&tool_id, key.clone(), cx)),
        );
    row = if selected {
        row.bg(theme.accent).text_color(theme.accent_foreground)
    } else {
        row.text_color(theme.foreground)
            .hover(|s| s.bg(theme.secondary))
    };
    row.child(icon(type_icon(&entry.kind), 14., theme.muted_foreground))
        .child(div().flex_1().min_w(px(0.)).truncate().child(entry.name()))
        .child(
            div()
                .flex_none()
                .text_color(theme.muted_foreground)
                .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                .child(ttl_short(entry.ttl_ms)),
        )
}

/// Two-column list for hash / list / set / sorted set values.
fn grid(
    headers: (&str, &str),
    rows: impl Iterator<Item = (String, String)>,
    total: u64,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let cell = |text: String, first: bool| {
        div()
            .when(first, |d| d.w(relative(0.32)).flex_none())
            .when(!first, |d| d.flex_1())
            .min_w(px(0.))
            .px(px(SPACE_2))
            .py(px(6.))
            .whitespace_normal()
            .child(clip(text, ITEM_SHOWN))
    };
    let mut table = div()
        .flex()
        .flex_col()
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .text_size(crate::theme::ui_size(theme, FONT_LABEL))
        .child(
            div()
                .flex()
                .bg(theme.secondary)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(cell(headers.0.into(), true))
                .child(cell(headers.1.into(), false)),
        );
    let mut shown = 0u64;
    for (i, (a, b)) in rows.enumerate() {
        shown += 1;
        table = table.child(
            div()
                .flex()
                .border_t_1()
                .border_color(theme.border)
                .bg(if i % 2 == 0 {
                    theme.table_even
                } else {
                    theme.table
                })
                .child(cell(a, true).text_color(theme.muted_foreground))
                .child(cell(b, false)),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .child(table)
        .when(total > shown, |c| {
            c.child(caption_text(
                format!(
                    "Showing the first {shown} of {total}. Use the console (HSCAN, LRANGE, SSCAN, ZRANGE) for the rest."
                ),
                theme,
            ))
        })
        .into_any_element()
}

fn format_score(score: f64) -> String {
    if score.fract() == 0.0 && score.abs() < 1e15 {
        format!("{score:.0}")
    } else {
        score.to_string()
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.),
        n => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
    }
}

/// The value as text for Copy value.
fn value_text(value: &KeyValue) -> String {
    match value {
        KeyValue::Str { bytes, .. } => display(bytes),
        KeyValue::Hash(pairs) => pairs
            .iter()
            .map(|(f, v)| format!("{}\t{}", display(f), display(v)))
            .collect::<Vec<_>>()
            .join("\n"),
        KeyValue::List(items) | KeyValue::Set(items) => items
            .iter()
            .map(|v| display(v))
            .collect::<Vec<_>>()
            .join("\n"),
        KeyValue::ZSet(items) => items
            .iter()
            .map(|(m, s)| format!("{}\t{}", format_score(*s), display(m)))
            .collect::<Vec<_>>()
            .join("\n"),
        KeyValue::Raw(text) => text.clone(),
        KeyValue::Missing => String::new(),
    }
}

#[allow(dead_code)]
fn _assert_send(_: &RedisSession) {}
