//! MailHog viewer: search kind dropdown, search box and the inbox in the
//! explorer column; the selected message (text, HTML as text, headers,
//! raw source, links, attachments) in the work area. The inbox refreshes
//! every few seconds while it is on screen.

use std::sync::Arc;
use std::time::Duration;

use devtools::mailhog::{MailHogClient, Message, MessagePage, SearchKind, PAGE};
use devtools::mime;
use ora_core::store::ToolInstance;

use super::tools::{env_tag, mail_client, resolve_password, spawn_tool_job, tool_icon};
use super::tools::{ConfirmAction, ToolConfirm};
use super::*;

const POLL: Duration = Duration::from_secs(5);
const BODY_SHOWN: usize = 200 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MailTab {
    Text,
    Html,
    Headers,
    Raw,
    /// The server Kilat started ("Start server from Kilat").
    Log,
}

pub(super) struct MailView {
    pub client: Option<Arc<MailHogClient>>,
    /// The last request reached MailHog.
    pub reachable: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub messages: Vec<Message>,
    pub total: u64,
    pub kind: SearchKind,
    pub search: Entity<InputState>,
    pub selected: Option<String>,
    pub tab: MailTab,
    pub epoch: u64,
    pub list_scroll: UniformListScrollHandle,
    pub polling: bool,
    _subs: Vec<Subscription>,
}

impl MailView {
    pub fn new(tool: &ToolInstance, window: &mut Window, cx: &mut Context<Workbench>) -> Self {
        let search = cx.new(|cx| InputState::new(&mut *window, cx).placeholder("Search mail"));
        let id = tool.id.clone();
        let subs = vec![cx.subscribe_in(
            &search,
            window,
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.mail_reload(&id, cx);
                }
            },
        )];
        MailView {
            client: None,
            reachable: false,
            loading: false,
            error: None,
            messages: Vec::new(),
            total: 0,
            kind: SearchKind::Containing,
            search,
            selected: None,
            tab: MailTab::Text,
            epoch: 0,
            list_scroll: UniformListScrollHandle::new(),
            polling: false,
            _subs: subs,
        }
    }

    fn selected_message(&self) -> Option<&Message> {
        let id = self.selected.as_deref()?;
        self.messages.iter().find(|m| m.id == id)
    }
}

/// "08:21" today, "Yesterday", else "21 Nov", in local time.
fn time_label(created: &str) -> String {
    use chrono::{DateTime, Local};
    let Ok(t) = DateTime::parse_from_rfc3339(created) else {
        return created.get(..10).unwrap_or(created).to_string();
    };
    let t = t.with_timezone(&Local);
    let today = Local::now().date_naive();
    if t.date_naive() == today {
        t.format("%H:%M").to_string()
    } else if today.pred_opt() == Some(t.date_naive()) {
        "Yesterday".into()
    } else {
        t.format("%-d %b").to_string()
    }
}

fn full_time(created: &str) -> String {
    use chrono::{DateTime, Local};
    DateTime::parse_from_rfc3339(created)
        .map(|t| {
            t.with_timezone(&Local)
                .format("%a %-d %b %Y, %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| created.to_string())
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Display name of an address: `Kilat <noreply@x>` → `Kilat`.
fn sender_name(from: &str) -> String {
    match from.split_once('<') {
        Some((name, _)) if !name.trim().is_empty() => name.trim().trim_matches('"').to_string(),
        _ => from.trim_matches(|c| c == '<' || c == '>').to_string(),
    }
}

impl Workbench {
    pub(super) fn mail_connect(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(tool) = self.tools.get(id).cloned() else {
            return;
        };
        let typed = self.tools.session_passwords.get(id).cloned();
        if let Some(view) = self.tools.mail.get_mut(id) {
            view.loading = true;
            view.error = None;
        }
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || {
                let password = resolve_password(&tool, typed);
                let client = mail_client(&tool, password);
                let page = client.list(0);
                (client, page)
            },
            move |this, (client, page), cx| {
                let Some(view) = this.tools.mail.get_mut(&id) else {
                    return;
                };
                view.client = Some(client);
                this.mail_apply(&id, 0, false, page);
                this.mail_start_poll(&id, cx);
            },
        );
        cx.notify();
    }

    /// Apply a page fetched with `start` (0 replaces the list).
    fn mail_apply(
        &mut self,
        id: &str,
        start: usize,
        append: bool,
        page: Result<MessagePage, String>,
    ) {
        let Some(view) = self.tools.mail.get_mut(id) else {
            return;
        };
        view.loading = false;
        match page {
            Ok(page) => {
                view.reachable = true;
                view.error = None;
                view.total = page.total;
                if append && start > 0 {
                    let seen: std::collections::HashSet<String> =
                        view.messages.iter().map(|m| m.id.clone()).collect();
                    view.messages
                        .extend(page.messages.into_iter().filter(|m| !seen.contains(&m.id)));
                } else {
                    view.messages = page.messages;
                }
                if view
                    .selected
                    .as_deref()
                    .is_some_and(|s| !view.messages.iter().any(|m| m.id == s))
                {
                    view.selected = None;
                }
            }
            Err(err) => {
                view.reachable = false;
                view.error = Some(err);
            }
        }
    }

    fn mail_fetch(&mut self, id: &str, start: usize, quiet: bool, cx: &mut Context<Self>) {
        let Some(view) = self.tools.mail.get_mut(id) else {
            return;
        };
        let Some(client) = view.client.clone() else {
            return;
        };
        if !quiet {
            view.loading = true;
        }
        if start == 0 && !quiet {
            view.epoch += 1;
        }
        let epoch = view.epoch;
        let kind = view.kind;
        let query = view.search.read(cx).value().trim().to_string();
        let id = id.to_string();
        spawn_tool_job(
            cx,
            move || {
                if query.is_empty() {
                    client.list(start)
                } else {
                    client.search(kind, &query, start)
                }
            },
            move |this, page, _| {
                if this.tools.mail.get(&id).is_none_or(|v| v.epoch != epoch) {
                    return;
                }
                this.mail_apply(&id, start, start > 0, page);
            },
        );
        cx.notify();
    }

    /// New search or kind: the list starts over.
    pub(super) fn mail_reload(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.tools.mail.get_mut(id) {
            view.messages.clear();
            view.selected = None;
        }
        self.mail_fetch(id, 0, false, cx);
    }

    /// Refresh the inbox every few seconds while it is on screen and only
    /// its first page is loaded. Stops when the tool is closed.
    fn mail_start_poll(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(view) = self.tools.mail.get_mut(id) else {
            return;
        };
        if view.polling {
            return;
        }
        view.polling = true;
        let id = id.to_string();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            let alive = this
                .update(cx, |this, cx| {
                    let Some(view) = this.tools.mail.get(&id) else {
                        return false;
                    };
                    let shown = this.tools.active.as_deref() == Some(id.as_str());
                    if shown && !view.loading && view.messages.len() <= PAGE {
                        this.mail_fetch(&id, 0, true, cx);
                    }
                    true
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    fn mail_select(&mut self, id: &str, message: String, cx: &mut Context<Self>) {
        if let Some(view) = self.tools.mail.get_mut(id) {
            view.selected = Some(message);
            if view.tab == MailTab::Log {
                view.tab = MailTab::Text;
            }
        }
        cx.notify();
    }

    /// Delete one message (`Some`) or all of them.
    pub(super) fn mail_delete(
        &mut self,
        id: &str,
        message: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.tools.get(id).is_none_or(ToolInstance::read_only) {
            return;
        }
        let Some(client) = self.tools.mail.get(id).and_then(|v| v.client.clone()) else {
            return;
        };
        let id = id.to_string();
        spawn_tool_job(
            cx,
            {
                let message = message.clone();
                move || match &message {
                    Some(m) => client.delete_message(m),
                    None => client.delete_all(),
                }
            },
            move |this, result, cx| {
                let Some(view) = this.tools.mail.get_mut(&id) else {
                    return;
                };
                match result {
                    Ok(()) => match message {
                        Some(m) => {
                            if let Some(pos) = view.messages.iter().position(|x| x.id == m) {
                                view.messages.remove(pos);
                                view.total = view.total.saturating_sub(1);
                                // Select the next message, as a mail app does.
                                view.selected = view
                                    .messages
                                    .get(pos.min(view.messages.len().saturating_sub(1)))
                                    .map(|x| x.id.clone());
                            }
                        }
                        None => {
                            view.messages.clear();
                            view.total = 0;
                            view.selected = None;
                        }
                    },
                    Err(err) => view.error = Some(err),
                }
                cx.notify();
            },
        );
    }

    fn mail_request_delete_all(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(view) = self.tools.mail.get(id) else {
            return;
        };
        self.tools.confirm = Some(ToolConfirm {
            title: "Delete all messages?".into(),
            text: format!(
                "All {} message(s) are deleted from MailHog. This cannot be undone.",
                view.total
            ),
            button: "Delete All",
            action: ConfirmAction::DeleteAllMail(id.to_string()),
        });
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Explorer column
    // -----------------------------------------------------------------

    pub(super) fn render_mail_explorer(
        &self,
        tool: &ToolInstance,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = self.tools.mail.get(&tool.id);
        let entity = cx.entity();
        let id = tool.id.clone();
        let live = view.is_some_and(|v| v.reachable);
        let read_only = tool.read_only();
        let has_mail = view.is_some_and(|v| !v.messages.is_empty());
        let header = {
            let (e1, e2, e3, e4) = (
                entity.clone(),
                entity.clone(),
                entity.clone(),
                entity.clone(),
            );
            let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
            let kind = tool.kind;
            let flags = self.server_flags(tool);
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
                        let menu = menu
                            .min_w(px(200.))
                            .item(
                                PopupMenuItem::new("Refresh")
                                    .icon(Icon::new(IconName::RefreshCw))
                                    .on_click(move |_, _, cx| {
                                        e1.update(cx, |this, cx| this.mail_fetch(&i1, 0, false, cx))
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Delete all messages…")
                                    .icon(Icon::new(IconName::Trash))
                                    .disabled(read_only || !has_mail)
                                    .on_click(move |_, _, cx| {
                                        e2.update(cx, |this, cx| {
                                            this.mail_request_delete_all(&i2, cx)
                                        })
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
                            );
                        let menu = tools_server::server_menu(menu, &i4, flags, &e4);
                        menu.separator().item(
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
        let current = view.kind;
        let kind_button = {
            let entity = entity.clone();
            let id = id.clone();
            explorer::control("mail-kind", theme)
                .w_full()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap(px(6.))
                        .child(icon(IconName::Inbox, 14., theme.muted_foreground))
                        .child(div().flex_1().text_left().child(current.label()))
                        .child(caption_text(view.total.to_string(), theme))
                        .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
                )
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.min_w(px(200.));
                    for kind in SearchKind::ALL {
                        let entity = entity.clone();
                        let id = id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(match kind {
                                SearchKind::Containing => "All mail (search anywhere)",
                                SearchKind::From => "Search by sender",
                                SearchKind::To => "Search by recipient",
                            })
                            .checked(kind == current)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    if let Some(v) = this.tools.mail.get_mut(&id) {
                                        v.kind = kind;
                                    }
                                    this.mail_reload(&id, cx);
                                })
                            }),
                        );
                    }
                    menu
                })
        };
        col = col.child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(SPACE_1))
                .px(px(SPACE_2))
                .pb(px(SPACE_2))
                .child(kind_button)
                .child(
                    Input::new(&view.search)
                        .small()
                        .prefix(icon(IconName::Search, 14., theme.muted_foreground))
                        .w_full(),
                ),
        );
        let list = if view.messages.is_empty() {
            let text = if let Some(err) = &view.error {
                err.clone()
            } else if !view.search.read(cx).value().trim().is_empty() {
                "No messages match.".into()
            } else {
                format!(
                    "No messages yet. Point your app's SMTP at {}:1025 and send one.",
                    tool.host
                )
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_3))
                .p(px(SPACE_4))
                .child(if view.loading {
                    loading_state("Loading messages…", theme)
                } else {
                    empty_state_text(text, theme).text_center()
                })
                .when(view.error.is_some() && !view.loading, |c| {
                    let id = id.clone();
                    c.children(self.start_server_button(tool, cx)).child(
                        Button::new("mail-retry")
                            .small()
                            .label("Try again")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.tools.mail.get(&id).is_some_and(|v| v.client.is_some()) {
                                    this.mail_fetch(&id, 0, false, cx)
                                } else {
                                    this.mail_connect(&id, cx)
                                }
                            })),
                    )
                })
                .into_any_element()
        } else {
            let rows: Vec<(String, String, String, String, String, bool)> = view
                .messages
                .iter()
                .map(|m| {
                    (
                        m.id.clone(),
                        sender_name(&m.from),
                        if m.subject.is_empty() {
                            "(no subject)".into()
                        } else {
                            m.subject.clone()
                        },
                        m.snippet(),
                        time_label(&m.created),
                        !m.attachments.is_empty(),
                    )
                })
                .collect();
            let count = rows.len();
            let selected = view.selected.clone();
            let theme2 = theme.clone();
            let id = id.clone();
            div()
                .flex_1()
                .min_h(px(0.))
                .px(px(SPACE_2))
                .child(
                    uniform_list(
                        "mail-list",
                        count,
                        cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .map(|ix| {
                                    let row = &rows[ix];
                                    let is_selected = selected.as_deref() == Some(row.0.as_str());
                                    render_mail_row(ix, row, is_selected, &id, &theme2, cx)
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
        let more = (view.messages.len() as u64) < view.total && !view.messages.is_empty();
        let id_more = id.clone();
        let loaded = view.messages.len();
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
                .child(
                    caption_text(
                        if more {
                            format!("{loaded} of {} messages", view.total)
                        } else {
                            format!("{} messages", view.total)
                        },
                        theme,
                    )
                    .flex_1(),
                )
                .when(more, |row| {
                    row.child(
                        Button::new("mail-more")
                            .xsmall()
                            .ghost()
                            .label("Load more")
                            .disabled(view.loading)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mail_fetch(&id_more, loaded, false, cx)
                            })),
                    )
                }),
        )
    }

    // -----------------------------------------------------------------
    // Work area
    // -----------------------------------------------------------------

    pub(super) fn render_mail_main(
        &self,
        tool: &ToolInstance,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = tool.id.clone();
        let Some(view) = self.tools.mail.get(&tool.id) else {
            return div().size_full().into_any_element();
        };
        let tab_button =
            |tab: MailTab, label: &'static str, enabled: bool, cx: &mut Context<Self>| {
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
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(v) = this.tools.mail.get_mut(&id) {
                            v.tab = tab;
                        }
                        if tab == MailTab::Log {
                            this.refresh_tool_log(&id);
                            this.tools.log_scroll.scroll_to_bottom();
                            this.start_log_poll(cx);
                        }
                        cx.notify();
                    }))
            };
        let server_tab = tools_server::can_start(tool);
        let message = view.selected_message();
        if message.is_none() || server_tab && view.tab == MailTab::Log {
            let showing_log = server_tab && view.tab == MailTab::Log;
            let body = if showing_log {
                self.render_server_log(tool, theme, cx).into_any_element()
            } else {
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(empty_state_text(
                        if view.messages.is_empty() {
                            "The inbox is empty.".into()
                        } else {
                            "Select a message to read it.".into()
                        },
                        theme,
                    ))
                    .into_any_element()
            };
            let has = message.is_some();
            return div()
                .flex()
                .flex_col()
                .size_full()
                .bg(theme.background)
                .when(server_tab, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .flex_none()
                            .gap(px(SPACE_3))
                            .px(px(SPACE_4))
                            .h(px(TOOLBAR_HEIGHT))
                            .border_b_1()
                            .border_color(theme.border)
                            .child(tab_button(MailTab::Text, "Text", has, cx))
                            .child(tab_button(MailTab::Html, "HTML", has, cx))
                            .child(tab_button(MailTab::Headers, "Headers", has, cx))
                            .child(tab_button(MailTab::Raw, "Raw", has, cx))
                            .child(tab_button(MailTab::Log, "Server log", true, cx)),
                    )
                })
                .child(body)
                .into_any_element();
        }
        let Some(message) = message else {
            return div().into_any_element();
        };
        let has_html = message.html.is_some();
        let has_text = message.text.is_some();
        // A tab without content falls back to the other body.
        let tab = match view.tab {
            MailTab::Html if !has_html => MailTab::Text,
            MailTab::Text if !has_text && has_html => MailTab::Html,
            t => t,
        };
        let read_only = tool.read_only();
        let message_id = message.id.clone();
        let id_delete = id.clone();
        let raw_copy = message.raw.clone();
        let toolbar = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_3))
            .px(px(SPACE_4))
            .h(px(TOOLBAR_HEIGHT))
            .border_b_1()
            .border_color(theme.border)
            .child(tab_button(MailTab::Text, "Text", has_text || !has_html, cx))
            .child(tab_button(MailTab::Html, "HTML", has_html, cx))
            .child(tab_button(MailTab::Headers, "Headers", true, cx))
            .child(tab_button(MailTab::Raw, "Raw", true, cx))
            .when(server_tab, |d| {
                d.child(tab_button(MailTab::Log, "Server log", true, cx))
            })
            .child(div().flex_1())
            .child(
                Button::new("mail-copy-raw")
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .tooltip("Copy raw source")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(raw_copy.clone()))
                    }),
            )
            .child(
                Button::new("mail-delete")
                    .small()
                    .ghost()
                    .icon(IconName::Trash)
                    .tooltip(if read_only {
                        "PROD is read only"
                    } else {
                        "Delete message"
                    })
                    .disabled(read_only)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mail_delete(&id_delete, Some(message_id.clone()), cx)
                    })),
            );
        let meta_row = |label: &str, value: String| {
            div()
                .flex()
                .gap(px(SPACE_2))
                .child(
                    div()
                        .w(px(44.))
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(label.to_string()),
                )
                .child(div().flex_1().min_w(px(0.)).child(value))
        };
        let mut header = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_1))
            .px(px(SPACE_4))
            .pt(px(SPACE_4))
            .pb(px(SPACE_3))
            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
            .child(
                div()
                    .text_size(crate::theme::ui_size(theme, FONT_HEADING_MD))
                    .line_height(crate::theme::ui_size(theme, LEADING_HEADING_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .pb(px(SPACE_1))
                    .child(if message.subject.is_empty() {
                        "(no subject)".to_string()
                    } else {
                        message.subject.clone()
                    }),
            )
            .child(meta_row("From", message.from.clone()))
            .child(meta_row("To", message.to.join(", ")));
        if !message.cc.is_empty() {
            header = header.child(meta_row("Cc", message.cc.join(", ")));
        }
        header = header.child(meta_row("Date", full_time(&message.created)));
        if !message.attachments.is_empty() {
            let mut chips = div().flex().flex_wrap().gap(px(SPACE_2)).pt(px(SPACE_1));
            for a in &message.attachments {
                chips = chips.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(SPACE_2))
                        .py(px(2.))
                        .rounded(px(RADIUS_SM))
                        .border_1()
                        .border_color(theme.border)
                        .child(icon(IconName::Paperclip, 12., theme.muted_foreground))
                        .child(a.name.clone())
                        .child(caption_text(human_size(a.size), theme)),
                );
            }
            header = header.child(chips);
        }
        let mono = theme.mono_font_family.clone();
        let body_text_div = |text: String, mono_font: bool| {
            div()
                .p(px(SPACE_3))
                .rounded(px(RADIUS_MD))
                .border_1()
                .border_color(theme.border)
                .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                .line_height(crate::theme::ui_size(theme, LEADING_BODY))
                .whitespace_normal()
                .when(mono_font, |d| d.font_family(mono.clone()))
                .child(clip(&text, BODY_SHOWN))
        };
        let mut body = div().flex().flex_col().gap(px(SPACE_3));
        match tab {
            MailTab::Text => {
                body = body.child(body_text_div(
                    message.text.clone().unwrap_or_else(|| "(empty)".into()),
                    false,
                ));
            }
            MailTab::Html => {
                let html = message.html.clone().unwrap_or_default();
                body = body
                    .child(body_text_div(mime::html_to_text(&html), false))
                    .child(caption_text(
                        "HTML shown as plain text. Copy the raw source to see the markup.".into(),
                        theme,
                    ));
            }
            MailTab::Headers => {
                let mut list = div()
                    .flex()
                    .flex_col()
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .text_size(crate::theme::ui_size(theme, FONT_LABEL));
                for (i, (k, v)) in message.headers.iter().enumerate() {
                    list = list.child(
                        div()
                            .flex()
                            .when(i > 0, |d| d.border_t_1())
                            .border_color(theme.border)
                            .bg(if i % 2 == 0 {
                                theme.table_even
                            } else {
                                theme.table
                            })
                            .child(
                                div()
                                    .w(relative(0.3))
                                    .flex_none()
                                    .px(px(SPACE_2))
                                    .py(px(6.))
                                    .text_color(theme.muted_foreground)
                                    .child(k.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .px(px(SPACE_2))
                                    .py(px(6.))
                                    .whitespace_normal()
                                    .child(clip(v, 4000)),
                            ),
                    );
                }
                body = body.child(list);
            }
            MailTab::Raw | MailTab::Log => {
                body = body.child(body_text_div(message.raw.clone(), true));
            }
        }
        if matches!(tab, MailTab::Text | MailTab::Html) {
            let links = message
                .html
                .as_deref()
                .map(mime::html_links)
                .unwrap_or_default();
            if !links.is_empty() {
                let mut list = div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_1))
                    .child(form_label("Links in this email", theme));
                for (i, url) in links.into_iter().take(30).enumerate() {
                    let open = url.clone();
                    list = list.child(
                        div()
                            .id(("mail-link", i))
                            .flex()
                            .items_center()
                            .gap(px(SPACE_2))
                            .px(px(SPACE_2))
                            .py(px(4.))
                            .rounded(px(RADIUS_SM))
                            .cursor_pointer()
                            .text_size(crate::theme::ui_size(theme, FONT_LABEL))
                            .text_color(theme.link)
                            .hover(|s| s.bg(theme.secondary))
                            .on_click(move |_, _, cx| cx.open_url(&open))
                            .child(icon(IconName::ExternalLink, 12., theme.muted_foreground))
                            .child(div().flex_1().min_w(px(0.)).truncate().child(url)),
                    );
                }
                body = body.child(list);
            }
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(theme.background)
            .child(toolbar)
            .child(header)
            .child(
                div()
                    .id("mail-body-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px(px(SPACE_4))
                    .pb(px(SPACE_4))
                    .child(body),
            )
            .into_any_element()
    }
}

fn render_mail_row(
    ix: usize,
    row: &(String, String, String, String, String, bool),
    selected: bool,
    tool_id: &str,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<Workbench>,
) -> impl IntoElement {
    let (message_id, from, subject, snippet, time, attachment) = row.clone();
    let tool_id = tool_id.to_string();
    let mut item = div()
        .id(("mail-row", ix))
        .w_full()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(1.))
        .h(px(62.))
        .px(px(SPACE_2))
        .rounded(px(RADIUS_SM))
        .text_size(crate::theme::ui_size(theme, FONT_LABEL))
        .line_height(crate::theme::ui_size(theme, LEADING_LABEL))
        .on_click(
            cx.listener(move |this, _, _, cx| this.mail_select(&tool_id, message_id.clone(), cx)),
        );
    item = if selected {
        item.bg(theme.accent).text_color(theme.accent_foreground)
    } else {
        item.text_color(theme.foreground)
            .hover(|s| s.bg(theme.secondary))
    };
    item.child(
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_2))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(from),
            )
            .when(attachment, |r| {
                r.child(icon(IconName::Paperclip, 12., theme.muted_foreground))
            })
            .child(
                div()
                    .flex_none()
                    .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
                    .text_color(theme.muted_foreground)
                    .child(time),
            ),
    )
    .child(div().truncate().child(subject))
    .child(
        div()
            .truncate()
            .text_size(crate::theme::ui_size(theme, FONT_CAPTION))
            .text_color(theme.muted_foreground)
            .child(snippet),
    )
}

fn human_size(n: usize) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.0} KB", n as f64 / 1024.),
        n => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
    }
}
