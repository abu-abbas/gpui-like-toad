use std::borrow::Cow;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    assets, div, img, px, size, AnyWindowHandle, App, AssetSource, Bounds, Context, FontWeight,
    Global, IntoElement, Render, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowOptions,
};

const APP_ICON: &str = "ora-app-icon.png";

pub(crate) struct OraAssets;

impl AssetSource for OraAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == APP_ICON {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../assets/icon/AppIcon.iconset/icon_256x256.png"
            ))));
        }
        assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = assets::AllAssets.list(path)?;
        if APP_ICON.starts_with(path) {
            paths.push(APP_ICON.into());
        }
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
            .child(content(theme))
    }
}

pub(crate) fn app_icon(size: f32) -> impl IntoElement {
    img(APP_ICON).size(px(size)).flex_none()
}

pub(crate) fn content(theme: &gpui_kit::component::Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_center()
        .gap(px(14.))
        .p(px(20.))
        .child(app_icon(76.))
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
                .child("Oracle database client built with GPUI and Rust"),
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
