use std::borrow::Cow;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    assets, div, img, px, size, AnyWindowHandle, App, AssetSource, Bounds, Context, FontWeight,
    Global, IntoElement, Render, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowOptions,
};

use ora_core::store::AppIcon;

const DESCRIPTION: &str = "Lightweight database client built with GPUI and Rust";
const FOLDED_ICON_PATH: &str = "kilat-app-icon.png";
const BOLT_ICON_PATH: &str = "kilat-bolt.png";

pub(crate) fn icon_png(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::Folded => include_bytes!("../assets/icon/AppIcon.iconset/icon_256x256.png"),
        AppIcon::Bolt => include_bytes!("../assets/icon/kilat-bolt.png"),
    }
}

pub(crate) struct KilatAssets;

impl AssetSource for KilatAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let icon = match path {
            FOLDED_ICON_PATH => AppIcon::Folded,
            BOLT_ICON_PATH => AppIcon::Bolt,
            _ => return assets::AllAssets.load(path),
        };
        Ok(Some(Cow::Borrowed(icon_png(icon))))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = assets::AllAssets.list(path)?;
        paths.extend(
            [FOLDED_ICON_PATH, BOLT_ICON_PATH]
                .into_iter()
                .filter(|name| name.starts_with(path))
                .map(SharedString::from),
        );
        Ok(paths)
    }
}

struct AboutWindow(AnyWindowHandle);
impl Global for AboutWindow {}

struct About;

impl Render for About {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(theme.background)
            .child(content(theme, cx))
    }
}

pub(crate) fn app_icon(size: f32, icon: AppIcon) -> impl IntoElement {
    let path = match icon {
        AppIcon::Folded => FOLDED_ICON_PATH,
        AppIcon::Bolt => BOLT_ICON_PATH,
    };
    img(path).size(px(size)).flex_none()
}

fn content(theme: &gpui_kit::component::Theme, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_center()
        .gap(px(14.))
        .p(px(20.))
        .child(app_icon(76., crate::theme::appearance(cx).app_icon))
        .child(
            div()
                .text_size(crate::theme::ui_size(theme, 22.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(app_name!()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .text_size(crate::theme::ui_size(theme, 13.))
                .text_color(theme.muted_foreground)
                .child(concat!("Version ", env!("CARGO_PKG_VERSION")))
                .child(DESCRIPTION),
        )
}

pub(crate) fn settings_header(theme: &gpui_kit::component::Theme, cx: &App) -> impl IntoElement {
    use crate::theme::{ui_size, FONT_HEADING_MD, FONT_LABEL, SPACE_2, SPACE_3};
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .child(app_icon(48., crate::theme::appearance(cx).app_icon))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .min_w(px(0.))
                .text_size(ui_size(theme, FONT_LABEL))
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(SPACE_2))
                        .child(
                            div()
                                .text_size(ui_size(theme, FONT_HEADING_MD))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(app_name!()),
                        )
                        .child(env!("CARGO_PKG_VERSION")),
                )
                .child(DESCRIPTION),
        )
}

pub(crate) fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<AboutWindow>().map(|about| about.0) {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(410.), px(320.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(concat!("About ", app_name!()).into()),
            ..Default::default()
        }),
        is_resizable: false,
        is_minimizable: false,
        app_id: Some("gpui-like-toad".into()),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| About)) {
        Ok((handle, _)) => cx.set_global(AboutWindow(handle)),
        Err(err) => eprintln!("error: failed to open About window: {err:#}"),
    }
}
