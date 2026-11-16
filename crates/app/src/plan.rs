//! Explain-plan tree rendering: indented rows with expand/collapse
//! chevrons, operation + object labels, and estimated cost on the right,
//! matching the design-system example (24px rows, 12px labels, 11px cost).

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, Entity, IntoElement};

use crate::theme::{FONT_CAPTION, FONT_LABEL, RADIUS_MD, SPACE_1, SPACE_2};
use crate::workbench::Workbench;
use ora_core::PlanNode;

/// Height of one plan-tree row (from the design image; the spec fixes no
/// token for plan rows, so the tree-row rhythm is followed, tightened).
const PLAN_ROW_HEIGHT: f32 = 24.0;
/// Indent per tree depth.
const PLAN_INDENT: f32 = 16.0;

/// Children of each node id, in plan order. Roots have key `None`.
fn children_map(nodes: &[PlanNode]) -> HashMap<Option<i64>, Vec<&PlanNode>> {
    let mut map: HashMap<Option<i64>, Vec<&PlanNode>> = HashMap::new();
    for node in nodes {
        map.entry(node.parent_id).or_default().push(node);
    }
    map
}

/// All node ids: a fresh plan starts fully expanded.
pub fn all_plan_ids(nodes: &[PlanNode]) -> HashSet<i64> {
    nodes.iter().map(|n| n.id).collect()
}

/// Render the plan tree. Clicking a parent row toggles its expanded state
/// in the workbench; leaf rows do nothing.
pub fn render_plan_tree(
    nodes: &[PlanNode],
    expanded: &HashSet<i64>,
    view: Entity<Workbench>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let map = children_map(nodes);
    let mut rows: Vec<gpui_kit::AnyElement> = Vec::new();
    if let Some(roots) = map.get(&None) {
        for root in roots {
            render_node(root, 0, &map, expanded, &view, &theme, &mut rows);
        }
    }
    // Orphaned nodes (parent id not in the list) still render, at depth 0,
    // so a malformed plan never silently drops steps.
    let known: HashSet<i64> = nodes.iter().map(|n| n.id).collect();
    for node in nodes {
        if let Some(parent) = node.parent_id {
            if !known.contains(&parent) {
                render_node(node, 0, &map, expanded, &view, &theme, &mut rows);
            }
        }
    }
    div()
        .flex()
        .flex_col()
        .w_full()
        .children(rows)
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    node: &PlanNode,
    depth: usize,
    map: &HashMap<Option<i64>, Vec<&PlanNode>>,
    expanded: &HashSet<i64>,
    view: &Entity<Workbench>,
    theme: &gpui_kit::component::Theme,
    rows: &mut Vec<gpui_kit::AnyElement>,
) {
    let children = map.get(&Some(node.id));
    let has_children = children.is_some_and(|c| !c.is_empty());
    let is_expanded = expanded.contains(&node.id);
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(PLAN_ROW_HEIGHT))
        .gap(px(SPACE_1))
        .pl(px(SPACE_2 + PLAN_INDENT * depth as f32))
        .pr(px(SPACE_2))
        .rounded(px(RADIUS_MD))
        .hover(|this| this.bg(theme.accent));
    if has_children {
        let icon = if is_expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        row = row.child(
            Icon::new(icon)
                .size(px(12.))
                .text_color(theme.muted_foreground),
        );
    } else {
        row = row.child(div().w(px(12.)).flex_none());
    }
    row = row
        .child(
            div()
                .text_size(px(FONT_LABEL))
                .line_height(px(18.))
                .text_color(theme.foreground)
                .child(node.label()),
        )
        .child(div().flex_1());
    if let Some(cost) = node.cost {
        let mut cost_text = format!("(Cost: {cost}");
        if let Some(card) = node.cardinality {
            cost_text.push_str(&format!(" · {card} rows"));
        }
        cost_text.push(')');
        row = row.child(
            div()
                .text_size(px(FONT_CAPTION))
                .line_height(px(16.))
                .text_color(theme.muted_foreground)
                .child(cost_text),
        );
    }
    if has_children {
        let node_id = node.id;
        let view = view.clone();
        row = row.on_click(move |_, _, cx: &mut App| {
            view.update(cx, |this, cx| {
                this.toggle_plan_node(node_id);
                cx.notify();
            });
        });
    }
    rows.push(row.into_any_element());
    if has_children && is_expanded {
        if let Some(children) = children {
            for child in children {
                render_node(child, depth + 1, map, expanded, view, theme, rows);
            }
        }
    }
}
