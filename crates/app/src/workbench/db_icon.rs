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
        DbKind::Postgres => include_bytes!("../../assets/db/postgres.svg"),
        DbKind::Mysql => include_bytes!("../../assets/db/mysql.svg"),
    }
}

/// Brand-like tint: coral red for Oracle, blue for SQLite. Slightly lighter
/// on dark themes so it stays readable.
fn color(kind: DbKind, theme: &Theme) -> Hsla {
    let dark = theme.background.l < 0.5;
    let (hue, saturation, lightness) = match (kind, dark) {
        (DbKind::Oracle, true) => (2., 0.85, 0.67),
        (DbKind::Oracle, false) => (2., 0.72, 0.47),
        (DbKind::Sqlite, true) => (200., 0.92, 0.58),
        (DbKind::Sqlite, false) => (200., 0.85, 0.39),
        (DbKind::Postgres, true) => (210., 0.60, 0.65),
        (DbKind::Postgres, false) => (210., 0.55, 0.42),
        (DbKind::Mysql, true) => (28., 0.85, 0.60),
        (DbKind::Mysql, false) => (28., 0.80, 0.42),
    };
    hsla(hue / 360., saturation, lightness, 1.)
}

pub(super) fn icon(kind: DbKind, connected: bool, theme: &Theme) -> Icon {
    Icon::default()
        .data(svg(kind))
        .size(gpui_kit::px(16.))
        .text_color(if connected {
            color(kind, theme)
        } else {
            theme.muted_foreground
        })
}

// Artwork for engines that are not supported yet. When `DbKind` gets
// `Mysql` / `Postgres`, move these into the `match`es above (suggested tints:
// MySQL teal ~190deg, PostgreSQL indigo ~225deg).
#[allow(dead_code)]
const MYSQL_SVG: &[u8] = include_bytes!("../../assets/db/mysql.svg");
#[allow(dead_code)]
const POSTGRES_SVG: &[u8] = include_bytes!("../../assets/db/postgres.svg");
