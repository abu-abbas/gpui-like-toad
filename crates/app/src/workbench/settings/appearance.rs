use super::*;

impl Workbench {
    pub(super) fn render_appearance(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let prefs = theme::appearance(cx);
        let appearance_mode = theme::preference(cx);
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(10.))
                    .bg(theme.table_even)
                    .child(setting_row(
                        "Mode",
                        "System follows your OS appearance",
                        mode,
                        theme,
                    ))
                    .child(setting_row(
                        if dark { "Dark theme" } else { "Light theme" },
                        if dark {
                            "Themes for dark mode"
                        } else {
                            "Themes for light mode"
                        },
                        self.render_theme_picker(dark, cx),
                        theme,
                    ))
                    .child(setting_row(
                        "Accent",
                        "",
                        self.render_accent_picker(&prefs.accent, theme, cx),
                        theme,
                    ))
                    .child(setting_row(
                        "Background",
                        "",
                        color_value(theme.background, theme),
                        theme,
                    ))
                    .child(setting_row(
                        "Foreground",
                        "",
                        color_value(theme.foreground, theme),
                        theme,
                    ))
                    .child(setting_row(
                        "UI font",
                        "",
                        body_text("System".into(), theme),
                        theme,
                    )),
            )
            .child(div().mt(px(SPACE_2)).child(form_label("Advanced", theme)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(10.))
                    .bg(theme.table_even)
                    .child(setting_row(
                        "UI font size",
                        "Base size for menus, lists and the data grid",
                        self.render_size_picker(false, prefs.ui_font_size, cx),
                        theme,
                    ))
                    .child(setting_row(
                        "SQL editor font size",
                        "JetBrains Mono in the SQL editor",
                        self.render_size_picker(true, prefs.sql_font_size, cx),
                        theme,
                    ))
                    .child(setting_row(
                        "Reduce motion",
                        "Limit animations across the workbench",
                        motion,
                        theme,
                    )),
            )
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
            .label(label)
            .icon(IconName::ChevronDown)
            .dropdown_menu(move |menu, window, cx| {
                let reset_view = view.clone();
                let theme_view = view.clone();
                let current = current.clone();
                let selected = current.is_none();
                let options = options.clone();
                menu.min_w(px(190.))
                    .item(
                        PopupMenuItem::new(theme::ORA_THEME_LABEL)
                            .checked(selected)
                            .on_click(move |_, window, cx| {
                                reset_view.update(cx, |this, cx| {
                                    this.set_theme_name(dark, None, window, cx)
                                });
                            }),
                    )
                    .submenu(
                        format!("gpui-kit themes ({})", options.len()),
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
            .small()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .child(status_dot(theme.primary))
                    .child(current)
                    .child(icon(IconName::ChevronDown, 14., theme.muted_foreground)),
            )
            .dropdown_menu(move |menu, _, _| {
                theme::ACCENTS.into_iter().fold(
                    menu.min_w(px(160.)),
                    |menu, (label, light, shade)| {
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::new(if label == "Violet" {
                                "Violet (default)"
                            } else {
                                label
                            })
                            .icon(
                                Icon::new(IconName::Circle).text_color(gpui_kit::rgb(if dark {
                                    shade
                                } else {
                                    light
                                })),
                            )
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
        Button::new(if editor {
            "sql-font-size"
        } else {
            "ui-font-size"
        })
        .small()
        .label(format!("{current} px"))
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
        })
    }
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
        .border_b_1()
        .border_color(theme.border)
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
        .child(
            div()
                .size(px(14.))
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .bg(color),
        )
        .child(body_text(label, theme))
}
