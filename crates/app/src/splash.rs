//! Launch splash: a small borderless card in the middle of the screen
//! that flashes the Kilat icon and plays a short thunder sound before the
//! main window opens. It is its own window so it stays small instead of filling
//! the 1280x800 main window.

use crate::about::app_icon;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, rgb, size, Animation, AnimationExt, App, Bounds, Context, FontWeight, IntoElement,
    Render, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use rodio::{Decoder, OutputStream, Sink};
use std::{io::Cursor, time::Duration};

/// How long the splash shows before the main window opens.
const SPLASH_TIME: Duration = Duration::from_millis(850);

struct Splash;

impl Render for Splash {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded(px(12.))
            .child(
                div()
                    .relative()
                    .size(px(48.))
                    .child(app_icon(48., crate::theme::appearance(cx).app_icon))
                    .when(!cx.reduce_motion(), |icon| {
                        let flash_color = rgb(0xe4d5ff);
                        icon.child(
                            div()
                                .absolute()
                                .top(px(3.))
                                .left(px(2.))
                                .w(px(42.))
                                .h(px(41.))
                                .rounded(px(11.))
                                .with_animation(
                                    "kilat-launch-flash",
                                    Animation::new(Duration::from_millis(290)),
                                    move |layer, progress| {
                                        let strength = if progress < 0.14 {
                                            progress / 0.14
                                        } else {
                                            ((1. - progress) / 0.86).powi(2)
                                        };
                                        layer.bg(flash_color.opacity(strength * 0.62))
                                    },
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(app_name!()),
            )
    }
}

/// Show the splash, then open the main window with `open_main` and close
/// the splash. Falls back to opening the main window right away when the
/// splash window cannot be created.
pub(crate) fn show_then(cx: &mut App, open_main: fn(&mut App)) {
    let pref = ora_core::store::load_theme_preference();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(240.), px(160.)),
            cx,
        ))),
        titlebar: None,
        kind: WindowKind::PopUp,
        is_resizable: false,
        is_minimizable: false,
        is_movable: false,
        focus: false,
        // Only the rounded card shows, not a square window behind it.
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("kilat".into()),
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        crate::theme::apply_preference(pref, window, cx);
        cx.new(|_| Splash)
    });
    let Ok((handle, _)) = opened else {
        open_main(cx);
        return;
    };
    cx.background_executor()
        .spawn(async { play_lightning_sound() })
        .detach();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SPLASH_TIME).await;
        cx.update(|cx| {
            // Open the main window first so the app never has zero windows.
            open_main(cx);
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        });
    })
    .detach();
}

fn play_lightning_sound() {
    let Ok((_stream, output)) = OutputStream::try_default() else {
        return;
    };
    let Ok(sink) = Sink::try_new(&output) else {
        return;
    };
    let lightning = include_bytes!("../assets/audio/lightning-start.wav");
    if let Ok(sound) = Decoder::new(Cursor::new(lightning.as_slice())) {
        sink.append(sound);
        sink.sleep_until_end();
    }
}
