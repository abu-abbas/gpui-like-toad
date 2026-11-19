//! Remembered panel layout.

use super::*;

/// Which panels of the work area the user minimized, remembered across
/// restarts: the Info panel under a data tab's grid and the Explain Plan
/// panel beside a SQL tab's results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelLayout {
    pub info_collapsed: bool,
    pub plan_collapsed: bool,
    /// Width in pixels of the Connections sidebar, `None` = default.
    pub sidebar_width: Option<u32>,
    /// Width in pixels of the explorer column, `None` = default.
    pub explorer_width: Option<u32>,
}

const INFO_COLLAPSED_KEY: &str = "info_panel_collapsed";
const PLAN_COLLAPSED_KEY: &str = "plan_panel_collapsed";
const SIDEBAR_WIDTH_KEY: &str = "sidebar_width";
const EXPLORER_WIDTH_KEY: &str = "explorer_width";

pub fn load_panel_layout_in(dir: &Path) -> PanelLayout {
    let Some(v) = read_json_file(&dir.join("settings.json")) else {
        return PanelLayout::default();
    };
    let flag = |key: &str| v.get(key).and_then(Json::as_bool).unwrap_or(false);
    let width = |key: &str| {
        v.get(key)
            .and_then(Json::as_u64)
            .and_then(|w| u32::try_from(w).ok())
    };
    PanelLayout {
        info_collapsed: flag(INFO_COLLAPSED_KEY),
        plan_collapsed: flag(PLAN_COLLAPSED_KEY),
        sidebar_width: width(SIDEBAR_WIDTH_KEY),
        explorer_width: width(EXPLORER_WIDTH_KEY),
    }
}

pub fn save_panel_layout_in(dir: &Path, layout: PanelLayout) -> std::io::Result<()> {
    let mut pairs: BTreeMap<String, Json> = match read_json_file(&dir.join("settings.json")) {
        Some(Json::Obj(pairs)) => pairs.into_iter().collect(),
        _ => BTreeMap::new(),
    };
    pairs.insert(INFO_COLLAPSED_KEY.into(), Json::Bool(layout.info_collapsed));
    pairs.insert(PLAN_COLLAPSED_KEY.into(), Json::Bool(layout.plan_collapsed));
    for (key, width) in [
        (SIDEBAR_WIDTH_KEY, layout.sidebar_width),
        (EXPLORER_WIDTH_KEY, layout.explorer_width),
    ] {
        match width {
            Some(w) => pairs.insert(key.into(), Json::Num(f64::from(w))),
            None => pairs.remove(key),
        };
    }
    write_json_file(
        dir,
        "settings.json",
        &Json::Obj(pairs.into_iter().collect()),
    )
}

pub fn load_panel_layout() -> PanelLayout {
    load_panel_layout_in(&config_dir())
}

pub fn save_panel_layout(layout: PanelLayout) -> std::io::Result<()> {
    save_panel_layout_in(&config_dir(), layout)
}
