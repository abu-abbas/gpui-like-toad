mod results;
mod schema;
mod workbench;
mod worker;

use gpui_kit::{application, assets, init, App, WindowOptions};

fn main() {
    application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            init(cx);
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| workbench::Workbench::new(window, cx))
            })
            .expect("failed to open window");
        });
}
