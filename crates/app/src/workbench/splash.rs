use super::Workbench;
use crate::{about::app_icon, theme};
use gpui_kit::component::Theme;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, Context, FontWeight, IntoElement, Window};
use rodio::{Decoder, OutputStream, Sink};
use std::{io::Cursor, time::Duration};

impl Workbench {
    pub(crate) fn show_launch_splash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.splash_visible = true;
        let view = cx.weak_entity();
        window.on_next_frame(move |_, cx| {
            cx.background_executor()
                .spawn(async { play_gear_sound() })
                .detach();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(850))
                    .await;
                let _ = view.update(cx, |this, cx| {
                    this.splash_visible = false;
                    cx.notify();
                });
            })
            .detach();
        });
    }
}

pub(super) fn screen(theme: &Theme) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(14.))
        .bg(theme.background)
        .child(app_icon(80.))
        .child(
            div()
                .text_size(theme::ui_size(theme, 22.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(app_name!()),
        )
}

fn play_gear_sound() {
    let Ok((_stream, output)) = OutputStream::try_default() else {
        return;
    };
    let Ok(sink) = Sink::try_new(&output) else {
        return;
    };
    let gear = include_bytes!("../../assets/audio/gear-start.wav");
    if let Ok(sound) = Decoder::new(Cursor::new(gear.as_slice())) {
        sink.append(sound);
        sink.sleep_until_end();
    }
}
