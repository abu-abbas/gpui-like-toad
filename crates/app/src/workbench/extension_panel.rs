//! Native renderer for the bounded extension UI protocol.

use std::collections::HashMap;
use std::time::Duration;

use ora_core::extension_ui::{
    ButtonVariant, Event, EventKind, InputValue, Node, Slot, Text, TextStyle, Tone,
};

use super::*;

struct Field {
    input: Entity<InputState>,
    _subscription: Subscription,
    change: Option<gpui_kit::Task<()>>,
}

struct Action<'a> {
    id: &'a Text,
    item: Option<&'a Text>,
    label: &'a Text,
    variant: Option<ButtonVariant>,
    disabled: bool,
    writes: bool,
    confirm: Option<&'a Text>,
}

pub(super) struct ExtensionPanel {
    slot: Slot,
    revision: u32,
    tree: Option<Node>,
    read_only: bool,
    production: bool,
    fields: HashMap<Text, Field>,
}

impl gpui_kit::EventEmitter<Event> for ExtensionPanel {}

impl ExtensionPanel {
    pub fn new(slot: Slot, read_only: bool, production: bool) -> Self {
        Self {
            slot,
            revision: 0,
            tree: None,
            read_only,
            production,
            fields: HashMap::new(),
        }
    }

    pub fn update(&mut self, revision: u32, tree: Option<Option<Node>>, cx: &mut Context<Self>) {
        self.revision = revision;
        if let Some(tree) = tree {
            self.tree = tree;
        }
        cx.notify();
    }

    fn send(&self, node: Text, event: EventKind, cx: &mut Context<Self>) {
        cx.emit(Event {
            revision: self.revision,
            slot: self.slot,
            node,
            event,
        });
    }

    fn init_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        fn collect<'a>(tree: &'a Node, fields: &mut Vec<(&'a Text, &'a Text, Option<&'a Text>)>) {
            match tree {
                Node::Input {
                    id,
                    value,
                    placeholder,
                } => fields.push((id, value, placeholder.as_ref())),
                Node::Column { children } | Node::Row { children } => {
                    for child in children {
                        collect(child, fields);
                    }
                }
                Node::Scroll { child } => collect(child, fields),
                Node::Tabs { items, .. } => {
                    for item in items {
                        collect(&item.child, fields);
                    }
                }
                _ => {}
            }
        }
        let mut entries = Vec::new();
        if let Some(tree) = &self.tree {
            collect(tree, &mut entries);
        }
        self.fields
            .retain(|id, _| entries.iter().any(|(key, _, _)| *key == id));
        for (id, value, placeholder) in entries {
            if self.fields.contains_key(id) {
                continue;
            }
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder.map(Text::as_str).unwrap_or_default().to_owned())
                    .default_value(value.as_str().to_owned())
            });
            let key = id.clone();
            let subscription = cx.subscribe_in(&input, window, move |this, input, event, _, cx| {
                let Ok(value) = Text::new(input.read(cx).value().to_string()) else {
                    return;
                };
                let Some(field) = this.fields.get_mut(&key) else {
                    return;
                };
                match event {
                    InputEvent::PressEnter { .. } => {
                        field.change = None;
                        this.send(key.clone(), EventKind::Submit { value }, cx);
                    }
                    InputEvent::Change => {
                        let key = key.clone();
                        field.change = Some(cx.spawn(async move |view, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(250))
                                .await;
                            let _ = view.update(cx, |this, cx| {
                                if this.fields.contains_key(&key) {
                                    this.send(
                                        key,
                                        EventKind::Change {
                                            value: InputValue::Text(value),
                                        },
                                        cx,
                                    );
                                }
                            });
                        }));
                    }
                    _ => {}
                }
            });
            self.fields.insert(
                id.clone(),
                Field {
                    input,
                    _subscription: subscription,
                    change: None,
                },
            );
        }
    }

    fn action(&self, action: Action<'_>, cx: &mut Context<Self>) -> Button {
        let Action {
            id,
            item,
            label,
            variant,
            disabled,
            writes,
            confirm,
        } = action;
        let key = id.clone();
        let item = item.cloned();
        let confirm = confirm.map(|text| text.as_str().to_owned());
        let button = Button::new(item.as_ref().unwrap_or(&key).as_str().to_owned())
            .small()
            .label(label.as_str().to_owned());
        let button = match variant {
            Some(ButtonVariant::Primary) => button.primary(),
            Some(ButtonVariant::Danger) => button.danger(),
            Some(ButtonVariant::Ghost) => button.ghost(),
            _ => button,
        };
        button
            .disabled(disabled || (writes && self.read_only))
            .on_click(cx.listener(move |this, _, window, cx| {
                let key = key.clone();
                let item = item.clone();
                let description = confirm
                    .as_deref()
                    .or((writes && this.production).then_some("Apply this change to production?"));
                if let Some(description) = description {
                    let revision = this.revision;
                    let prompt = window.prompt(
                        gpui_kit::PromptLevel::Warning,
                        description,
                        None,
                        &["Cancel", "Continue"],
                        cx,
                    );
                    cx.spawn(async move |view, cx| {
                        if prompt.await == Ok(1) {
                            let _ = view.update(cx, |this, cx| {
                                if this.revision == revision {
                                    this.send(key, EventKind::Click { item }, cx);
                                }
                            });
                        }
                    })
                    .detach();
                } else {
                    this.send(key, EventKind::Click { item }, cx);
                }
            }))
    }

    fn render_node(
        &self,
        tree: &Node,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        match tree {
            Node::Column { children } | Node::Row { children } => {
                let row = div().flex().min_w(px(0.)).min_h(px(0.)).gap(px(SPACE_2));
                let row = if matches!(tree, Node::Column { .. }) {
                    row.flex_col().flex_1()
                } else {
                    row.flex_wrap()
                };
                row.children(
                    children
                        .iter()
                        .map(|child| self.render_node(child, theme, cx)),
                )
                .into_any_element()
            }
            Node::Scroll { child } => div()
                .id("extension-scroll")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(self.render_node(child, theme, cx))
                .into_any_element(),
            Node::Spacer => div().flex_1().into_any_element(),
            Node::Divider => div().h(px(1.)).bg(theme.border).into_any_element(),
            Node::Text {
                text,
                style,
                tone,
                selectable,
            } => {
                let size = match style {
                    Some(TextStyle::Heading) => 18.,
                    Some(TextStyle::Caption) => 11.,
                    _ => 13.,
                };
                let text_div = div()
                    .text_size(crate::theme::ui_size(theme, size))
                    .text_color(color(*tone, theme));
                let text_div = if matches!(style, Some(TextStyle::Mono)) {
                    text_div.font_family(theme.mono_font_family.clone())
                } else {
                    text_div
                };
                let value = text.as_str().to_owned();
                let body = text_div.child(value.clone());
                if *selectable {
                    body.child(
                        Button::new("copy-text")
                            .small()
                            .ghost()
                            .label("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                    value.clone(),
                                ));
                            }),
                    )
                    .into_any_element()
                } else {
                    body.into_any_element()
                }
            }
            Node::Badge { text, tone } => div()
                .text_color(color(*tone, theme))
                .child(text.as_str().to_owned())
                .into_any_element(),
            Node::Code { text, copy, .. } => {
                let content = text.as_str().to_owned();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .children(copy.then(|| {
                        let content = content.clone();
                        Button::new("copy-code")
                            .small()
                            .ghost()
                            .label("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                    content.clone(),
                                ));
                            })
                    }))
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(content),
                    )
                    .into_any_element()
            }
            Node::Icon { name } => interface_icon(name)
                .map(|name| Icon::new(name).into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Node::Link { label, url } => {
                let url = url.as_str().to_owned();
                Button::new("extension-link")
                    .small()
                    .ghost()
                    .label(label.as_str().to_owned())
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
            Node::Button {
                id,
                label,
                variant,
                disabled,
                writes,
                confirm,
            } => self
                .action(
                    Action {
                        id,
                        item: None,
                        label,
                        variant: *variant,
                        disabled: *disabled,
                        writes: *writes,
                        confirm: confirm.as_ref(),
                    },
                    cx,
                )
                .into_any_element(),
            Node::Input { id, .. } => Input::new(&self.fields[id].input)
                .small()
                .w_full()
                .into_any_element(),
            Node::Select { id, value, options } => {
                let title = options
                    .iter()
                    .find(|(key, _)| key == value)
                    .map(|(_, label)| label.as_str().to_owned())
                    .unwrap_or_default();
                let entity = cx.entity();
                let id = id.clone();
                let options = options.clone();
                let value = value.clone();
                Button::new(id.as_str().to_owned())
                    .small()
                    .ghost()
                    .child(title)
                    .child(icon(IconName::ChevronDown, 12., theme.muted_foreground))
                    .dropdown_menu(move |mut menu, _, _| {
                        for (key, label) in &options {
                            let entity = entity.clone();
                            let id = id.clone();
                            let next = key.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label.as_str().to_owned())
                                    .checked(&value == key)
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.send(
                                                id.clone(),
                                                EventKind::Change {
                                                    value: InputValue::Text(next.clone()),
                                                },
                                                cx,
                                            );
                                        });
                                    }),
                            );
                        }
                        menu
                    })
                    .into_any_element()
            }
            Node::Checkbox { id, label, value } => {
                let node = id.clone();
                Checkbox::new(id.as_str().to_owned())
                    .label(label.as_str().to_owned())
                    .checked(*value)
                    .on_click(cx.listener(move |this, value: &bool, _, cx| {
                        this.send(
                            node.clone(),
                            EventKind::Change {
                                value: InputValue::Bool(*value),
                            },
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            Node::Menu { id, label, items } => div()
                .flex()
                .flex_wrap()
                .gap(px(SPACE_1))
                .child(label.as_str().to_owned())
                .children(items.iter().map(|item| {
                    self.action(
                        Action {
                            id,
                            item: Some(&item.id),
                            label: &item.label,
                            variant: Some(ButtonVariant::Ghost),
                            disabled: item.disabled,
                            writes: item.writes,
                            confirm: item.confirm.as_ref(),
                        },
                        cx,
                    )
                }))
                .into_any_element(),
            Node::Tabs { id, active, items } => {
                let buttons = div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_1))
                    .children(items.iter().map(|item| {
                        let node = id.clone();
                        let key = item.id.clone();
                        Button::new(item.id.as_str().to_owned())
                            .small()
                            .ghost()
                            .label(item.label.as_str().to_owned())
                            .selected(&item.id == active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send(node.clone(), EventKind::Tab { item: key.clone() }, cx)
                            }))
                    }));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_2))
                    .child(buttons)
                    .children(
                        items
                            .iter()
                            .find(|item| &item.id == active)
                            .map(|item| self.render_node(&item.child, theme, cx)),
                    )
                    .into_any_element()
            }
            Node::List { id, rows, more } => {
                let node = id.clone();
                let rows: Vec<_> = rows
                    .iter()
                    .map(|row| {
                        (
                            row.id.clone(),
                            row.title.as_str().to_owned(),
                            row.subtitle.as_ref().map(|s| s.as_str().to_owned()),
                            row.selected,
                        )
                    })
                    .collect();
                let count = rows.len();
                let list = uniform_list(
                    "extension-items",
                    count,
                    cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let (key, title, subtitle, selected) = &rows[index];
                                let node = node.clone();
                                let item = key.clone();
                                let label = subtitle
                                    .as_ref()
                                    .map_or_else(|| title.clone(), |s| format!("{title} · {s}"));
                                let label = if title.is_empty() {
                                    "(No subject)".to_owned()
                                } else {
                                    label
                                };
                                Button::new(index)
                                    .w_full()
                                    .small()
                                    .ghost()
                                    .selected(*selected)
                                    .label(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send(
                                            node.clone(),
                                            EventKind::Select { item: item.clone() },
                                            cx,
                                        )
                                    }))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full();
                let node = id.clone();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(80.))
                    .child(div().flex_1().min_h(px(0.)).child(list))
                    .children(more.then(|| {
                        Button::new("extension-more")
                            .small()
                            .ghost()
                            .label("Load more")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send(node.clone(), EventKind::More, cx)
                            }))
                    }))
                    .into_any_element()
            }
            Node::Table { columns, rows } => div()
                .flex()
                .flex_col()
                .gap(px(SPACE_1))
                .child(text_row(columns.iter()))
                .children(rows.iter().map(|row| text_row(row.iter())))
                .into_any_element(),
            Node::Kv { rows } => div()
                .flex()
                .flex_col()
                .gap(px(SPACE_1))
                .children(rows.iter().map(|(k, v)| text_row([k, v])))
                .into_any_element(),
            Node::Empty { text } => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(text.as_str().to_owned())
                .into_any_element(),
        }
    }
}

fn text_row<'a>(cells: impl IntoIterator<Item = &'a Text>) -> gpui_kit::Div {
    div().flex().gap(px(SPACE_2)).children(
        cells
            .into_iter()
            .map(|cell| div().flex_1().min_w(px(0.)).child(cell.as_str().to_owned())),
    )
}

fn color(tone: Option<Tone>, theme: &gpui_kit::component::Theme) -> Hsla {
    match tone {
        Some(Tone::Muted) => theme.muted_foreground,
        Some(Tone::Primary) => theme.primary,
        Some(Tone::Danger) => theme.danger,
        Some(Tone::Warning) => theme.warning,
        Some(Tone::Success) => theme.success,
        _ => theme.foreground,
    }
}

fn interface_icon(name: &Text) -> Option<IconName> {
    match name.as_str() {
        "search" => Some(IconName::Search),
        "trash" => Some(IconName::Trash),
        "refresh" => Some(IconName::RefreshCw),
        _ => None,
    }
}

impl Render for ExtensionPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.init_fields(window, cx);
        let theme = cx.theme().clone();
        if self.slot == Slot::Sidebar {
            return div()
                .text_size(crate::theme::ui_size(&theme, FONT_CAPTION))
                .children(
                    self.tree
                        .as_ref()
                        .map(|tree| self.render_node(tree, &theme, cx)),
                )
                .into_any_element();
        }
        div()
            .id("extension-panel")
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.))
            .min_w(px(0.))
            .p(px(SPACE_3))
            .overflow_y_scroll()
            .children(
                self.tree
                    .as_ref()
                    .map(|tree| self.render_node(tree, &theme, cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn polling_keeps_the_unsubmitted_search_draft(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(|_, _| ExtensionPanel::new(Slot::Explorer, false, false));
        window
            .update(cx, |panel, window, cx| {
                let id = Text::new("search".into()).unwrap();
                let tree = || Node::Input {
                    id: id.clone(),
                    value: Text::new("server value".into()).unwrap(),
                    placeholder: None,
                };
                panel.update(1, Some(Some(tree())), cx);
                panel.init_fields(window, cx);
                let input = panel.fields[&id].input.clone();
                input.update(cx, |state, cx| state.set_value("local draft", window, cx));
                panel.update(2, Some(Some(tree())), cx);
                panel.init_fields(window, cx);
                let field = &panel.fields[&id].input;
                assert_eq!(field.entity_id(), input.entity_id());
                assert_eq!(field.read(cx).value().as_ref(), "local draft");

                panel.update(3, Some(None), cx);
                panel.init_fields(window, cx);
                assert!(panel.fields.is_empty());
            })
            .unwrap();
    }
}
