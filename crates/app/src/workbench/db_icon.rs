//! Per-engine connection icons (Oracle pill, SQLite feather).
//!
//! To add an engine: drop `assets/db/<engine>.svg` (monochrome, `currentColor`)
//! and add one arm to each `match` below.
use gpui_kit::component::{Icon, Theme};
use gpui_kit::prelude::*;
use gpui_kit::{hsla, Hsla};
use ora_core::dialect::DbKind;

/// Bundled SVG for an engine.
fn svg(kind: DbKind) -> &'static [u8] {
    match kind {
        DbKind::Oracle => include_bytes!("../../assets/db/oracle.svg"),
        DbKind::Sqlite => include_bytes!("../../assets/db/sqlite.svg"),
    }
}

/// Brand-like tint: coral red for Oracle, blue for SQLite. Slightly lighter
/// on dark themes so it stays readable.
pub(super) fn color(kind: DbKind, theme: &Theme) -> Hsla {
    let dark = theme.background.l < 0.5;
    let lightness = if dark { 0.65 } else { 0.5 };
    let hue = match kind {
        DbKind::Oracle => 6. / 360.,
        DbKind::Sqlite => 210. / 360.,
    };
    hsla(hue, 0.78, lightness, 1.)
}

pub(super) fn icon(kind: DbKind, size: f32, theme: &Theme) -> Icon {
    Icon::default()
        .data(svg(kind))
        .size(gpui_kit::px(size))
        .text_color(color(kind, theme))
}
