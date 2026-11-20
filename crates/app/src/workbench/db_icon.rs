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
    let (hue, saturation, lightness) = match (kind, dark) {
        (DbKind::Oracle, true) => (8., 0.62, 0.66),
        (DbKind::Oracle, false) => (8., 0.68, 0.56),
        (DbKind::Sqlite, true) => (212., 0.80, 0.68),
        (DbKind::Sqlite, false) => (212., 0.80, 0.52),
    };
    hsla(hue / 360., saturation, lightness, 1.)
}

pub(super) fn icon(kind: DbKind, size: f32, theme: &Theme) -> Icon {
    Icon::default()
        .data(svg(kind))
        .size(gpui_kit::px(size))
        .text_color(color(kind, theme))
}

// Artwork for engines that are not supported yet. When `DbKind` gets
// `Mysql` / `Postgres`, move these into the `match`es above (suggested tints:
// MySQL teal ~190deg, PostgreSQL indigo ~225deg).
#[allow(dead_code)]
const MYSQL_SVG: &[u8] = include_bytes!("../../assets/db/mysql.svg");
#[allow(dead_code)]
const POSTGRES_SVG: &[u8] = include_bytes!("../../assets/db/postgres.svg");
