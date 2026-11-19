use super::*;
use ora_core::store::AppIcon;

impl Workbench {
    pub(super) fn render_appearance(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let prefs = theme::appearance(cx);
        let appearance_mode = theme::preference(cx);
        let mut icons = div().flex().gap(px(SPACE_2));
        for (icon, label) in [(AppIcon::Folded, "K Folded"), (AppIcon::Bolt, "K Bolt")] {
            icons = icons.child(
                Button::new(label)
                    .ghost()
                    .when(icon == prefs.app_icon, |button| button.bg(theme.secondary))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_1))
                            .child(crate::about::app_icon(24., icon))
                            .child(label),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.update_appearance(window, cx, |prefs| prefs.app_icon = icon);
                        cx.refresh_windows();
                    })),
            );
        }
        let mut mode = div()
            .flex()
            .p(px(2.))
            .rounded(px(RADIUS_MD))
            .bg(theme.secondary);
        for (pref, label) in [
            (ThemePreference::System, "System"),
            (ThemePreference::Light, "Light"),
            (ThemePreference::Dark, "Dark"),
        ] {
            mode = mode.child(
                Button::new(format!("appearance-mode-{label}"))
                    .small()
                    .ghost()
                    .label(label)
                    .when(pref == appearance_mode, |button| {
                        button.bg(theme.background)
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_theme(pref, window, cx)),
                    ),
            );
        }
        let dark = theme.mode.is_dark();
        let mut motion = div()
            .flex()
            .p(px(2.))
            .rounded(px(RADIUS_MD))
            .bg(theme.secondary);
        for (value, label) in [(None, "System"), (Some(true), "On"), (Some(false), "Off")] {
            motion = motion.child(
                Button::new(format!("motion-mode-{label}"))
                    .small()
                    .ghost()
                    .label(label)
                    .when(value == prefs.reduce_motion, |button| {
                        button.bg(theme.background)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.update_appearance(window, cx, |prefs| prefs.reduce_motion = value);
                    })),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(form_label("Visual style", theme))
            .child(setting_group(
                theme,
                [
                    setting_row("App Icon", "", icons, theme),
                    setting_row("Mode", "System follows your OS appearance", mode, theme),
                    setting_row(
                        if dark { "Dark theme" } else { "Light theme" },
                        if dark {
                            "Themes for dark mode"
                        } else {
                            "Themes for light mode"
                        },
                        self.render_theme_picker(dark, cx),
                        theme,
                    ),
                    setting_row(
                        "Accent",
                        "",
                        self.render_accent_picker(&prefs.accent, theme, cx),
                        theme,
                    ),
                    setting_row(
                        "Background",
                        "",
                        color_value(theme.background, theme),
                        theme,
                    ),
                    setting_row(
                        "Foreground",
                        "",
                        color_value(theme.foreground, theme),
                        theme,
                    ),
                    setting_row(
                        "UI font",
                        "",
                        self.render_font_picker(prefs.ui_font.clone(), cx),
                        theme,
                    ),
                ],
            ))
            .child(div().mt(px(SPACE_2)).child(form_label("Advanced", theme)))
            .child(setting_group(
                theme,
                [
                    setting_row(
                        "UI font size",
                        "Base size for menus, lists and the data grid",
                        self.render_size_picker(false, prefs.ui_font_size, cx),
                        theme,
                    ),
                    setting_row(
                        "SQL editor font size",
                        "JetBrains Mono in the SQL editor",
                        self.render_size_picker(true, prefs.sql_font_size, cx),
                        theme,
                    ),
                    setting_row(
                        "Reduce motion",
                        "Limit animations across the workbench",
                        motion,
                        theme,
                    ),
                ],
            ))
    }

    fn render_font_picker(
        &self,
        current: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        Button::new("ui-font-family")
            .small()
            .rounded(px(7.))
            .h(px(32.))
            .max_w(px(174.))
            .label(current.clone().unwrap_or_else(|| "System".into()))
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, cx| {
                let fonts = std::iter::once(None)
                    .chain(cx.text_system().all_font_names().into_iter().map(Some));
                fonts.fold(
                    menu.scrollable(true).max_h(px(320.)).min_w(px(200.)),
                    |menu, font| {
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::new(font.clone().unwrap_or_else(|| "System".into()))
                                .checked(font == current)
                                .on_click(move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.update_appearance(window, cx, |prefs| {
                                            prefs.ui_font = font.clone()
                                        })
                                    })
                                }),
                        )
                    },
                )
            })
    }

    fn render_theme_picker(&self, dark: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let names = theme::theme_names(cx);
        let options = theme::theme_names_for(dark, cx);
        let current =
            (if dark { names.dark } else { names.light }).filter(|name| options.contains(name));
        let label = current
            .as_deref()
            .unwrap_or(theme::ORA_THEME_LABEL)
            .to_owned();
        let view = cx.entity();
        Button::new("theme-picker")
            .small()
            .h(px(32.))
            .w(px(174.))
            .rounded(px(7.))
            .child(select_trigger(label, cx.theme()))
            .dropdown_menu(move |menu, window, cx| {
                let reset_view = view.clone();
                let theme_view = view.clone();
                let current = current.clone();
                let selected = current.is_none();
                let options = options.clone();
                menu.min_w(px(252.))
                    .check_side(gpui_kit::component::Side::Right)
                    .item(
                        PopupMenuItem::new(theme::ORA_THEME_LABEL)
                            .icon(icon(IconName::CaseSensitive, 20., cx.theme().primary))
                            .checked(selected)
                            .on_click(move |_, window, cx| {
                                reset_view.update(cx, |this, cx| {
                                    this.set_theme_name(dark, None, window, cx)
                                });
                            }),
                    )
                    .submenu_with_icon(
                        Some(icon(
                            IconName::CaseSensitive,
                            20.,
                            cx.theme().muted_foreground,
                        )),
                        format!("gpui-kit themes    {}", options.len()),
                        window,
                        cx,
                        move |menu, _, _| {
                            let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(190.));
                            for name in &options {
                                let view = theme_view.clone();
                                let pick = name.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(name.clone())
                                        .checked(current.as_ref() == Some(name))
                                        .on_click(move |_, window, cx| {
                                            view.update(cx, |this, cx| {
                                                this.set_theme_name(
                                                    dark,
                                                    Some(pick.clone()),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }),
                                );
                            }
                            menu
                        },
                    )
            })
    }

    fn render_accent_picker(
        &self,
        name: &str,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current = theme::ACCENTS
            .iter()
            .find(|(label, _, _)| *label == name)
            .unwrap_or(&theme::ACCENTS[0])
            .0;
        let view = cx.entity();
        let dark = theme.is_dark();
        Button::new("accent-picker")
            .h(px(32.))
            .w(px(174.))
            .rounded(px(7.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .w_full()
                    .child(swatch(theme.primary, theme))
                    .child(div().flex_1().child(current))
                    .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
            )
            .dropdown_menu(move |menu, _, _| {
                theme::ACCENTS.into_iter().fold(
                    menu.min_w(px(232.))
                        .check_side(gpui_kit::component::Side::Right),
                    |menu, (label, light, shade)| {
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                div()
                                    .flex()
                                    .flex_1()
                                    .items_center()
                                    .gap(px(10.))
                                    .h(px(32.))
                                    .child(swatch(
                                        gpui_kit::rgb(if dark { shade } else { light }).into(),
                                        cx.theme(),
                                    ))
                                    .child(if label == "Violet" {
                                        "Violet (default)"
                                    } else {
                                        label
                                    })
                            })
                            .checked(label == current)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.update_appearance(window, cx, |prefs| {
                                        prefs.accent = label.into()
                                    });
                                });
                            }),
                        )
                    },
                )
            })
    }

    fn render_size_picker(
        &self,
        editor: bool,
        current: u8,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        let picker = Button::new(if editor {
            "sql-font-size"
        } else {
            "ui-font-size"
        })
        .small()
        .w(px(86.))
        .h(px(32.))
        .rounded(px(7.))
        .child(div().w_full().child(current.to_string()))
        .dropdown_menu(move |menu, _, _| {
            (10..=20).fold(menu, |menu, size| {
                let view = view.clone();
                menu.item(
                    PopupMenuItem::new(format!("{size} px"))
                        .checked(current == size)
                        .on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| {
                                this.update_appearance(window, cx, |prefs| {
                                    if editor {
                                        prefs.sql_font_size = size;
                                    } else {
                                        prefs.ui_font_size = size;
                                    }
                                })
                            });
                        }),
                )
            })
        });
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(picker)
            .child(caption_text("px".into(), cx.theme()))
    }
}

fn setting_group(
    theme: &gpui_kit::component::Theme,
    rows: impl IntoIterator<Item = gpui_kit::Div>,
) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .bg(theme.table_even)
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            row.when(index > 0, |row| row.border_t_1().border_color(theme.border))
        }))
}

fn setting_row(
    label: &'static str,
    description: &'static str,
    control: impl IntoElement,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .min_h(px(48.))
        .gap(px(SPACE_2))
        .px(px(SPACE_4))
        .py(px(SPACE_1))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .child(body_text(label.into(), theme))
                .when(!description.is_empty(), |row| {
                    row.child(caption_text(description.into(), theme))
                }),
        )
        .child(div().flex_none().child(control))
}

fn color_value(color: Hsla, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    let rgb = color.to_rgb();
    let channel = |value: f32| (value * 255.).round() as u8;
    let label = format!(
        "#{:02X}{:02X}{:02X}",
        channel(rgb.r),
        channel(rgb.g),
        channel(rgb.b)
    );
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_2))
        .h(px(32.))
        .px(px(10.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .rounded(px(8.))
        .child(swatch(color, theme))
        .child(body_text(label, theme))
}

fn swatch(color: Hsla, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    div()
        .flex_none()
        .size(px(16.))
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(color)
}
