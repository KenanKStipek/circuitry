//! The `tokens by key` treemap's own layout (DESIGN.md §6.3, issue
//! #527): pure integer geometry, built here so `tui.rs` only ever
//! draws rectangles this module already decided, never computes one
//! of its own (§6.1's "no terminal code" split).
//!
//! The packing itself is the standard squarified treemap algorithm
//! (Bruls, Huizing & van Wijk, 2000): each row is grown, greedily, for
//! as long as doing so keeps the *worst* aspect ratio in that row no
//! worse than before, then laid out along whichever side of the
//! current rectangle is shorter. Row boundaries are computed once in
//! `f64`, so every cell in a row shares the exact same boundary value
//! as its neighbour; only then are boundaries rounded to integers
//! (`f64::round`, applied identically to every boundary), which is
//! what keeps the integer rectangles gap- and overlap-free — rounding
//! each cell's width independently instead would not.

use std::collections::BTreeMap;

use crate::model::StatusKind;

/// One key's own input to the treemap: its total (area-determining)
/// weight, a status to colour it by, and its own children, if any
/// (DESIGN.md §6.3: "items contain items"). A key with `total == 0`
/// must not be passed in at all — [`layout`] has no way to draw a
/// zero-area cell and does not special-case one; the caller (issue
/// #527: "keys with zero tokens get no cell") filters before calling.
#[derive(Debug, Clone, PartialEq)]
pub struct TreemapNode {
    pub key: String,
    /// The full dotted state path (`"prime.summarize"`) — lets a
    /// front end highlight the cell for whatever row is selected in
    /// the orchestration pane (issue #527) without having to
    /// reconstruct a path from nested keys alone.
    pub path: String,
    pub total: u64,
    pub status: Option<StatusKind>,
    pub children: Vec<TreemapNode>,
}

impl TreemapNode {
    pub fn leaf(key: impl Into<String>, total: u64, status: Option<StatusKind>) -> Self {
        let key = key.into();
        TreemapNode {
            path: key.clone(),
            key,
            total,
            status,
            children: Vec::new(),
        }
    }
}

/// One drawn cell (DESIGN.md §6.3). `merged` cells (the single `…`
/// bucket a parent's too-small children are folded into) have no
/// `children` of their own in the output — there is nothing further
/// to recurse into once several keys have been folded into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreemapCell {
    /// The key, or `"…"` for a merged bucket.
    pub label: String,
    /// The full dotted state path, or `None` for a merged bucket
    /// (several paths folded into one — no single path names it).
    pub path: Option<String>,
    pub rect: Rect,
    pub total: u64,
    pub status: Option<StatusKind>,
    /// Whether this cell has its own children laid out inside it
    /// (drawn with a border and a title, DESIGN.md §6.3) — `false`
    /// for a leaf, a merged bucket, or a container whose rect is too
    /// small to show a border and still leave room for anything
    /// inside it.
    pub has_children: bool,
    pub merged: bool,
    pub depth: u16,
}

/// A rect too small to hold a border *and* at least one row/column of
/// interior for its children (issue #527's "less than 2×1 inside
/// their parent" governs leaf cells; a container additionally needs
/// the border itself, one cell on every side).
const MIN_BORDER_RECT: (u16, u16) = (3, 3);

/// A rect this small is "too small to draw at all" (issue #527) and
/// gets folded into its parent's merged `…` bucket instead.
fn too_small(w: u16, h: u16) -> bool {
    w < 2 || h < 1
}

/// Lays `children` out inside `(x, y, w, h)`, recursing into any
/// child that itself has children and room for a border. Returns a
/// flat list: every surviving cell, in the same deterministic order
/// [`sorted_children`] produces (and, for a container, immediately
/// followed by its own children, depth-first) — the same order on
/// every call for the same input (issue #527's "the same input always
/// gives the same rectangles"), since neither the sort nor the
/// packing algorithm itself consult anything beyond `children` and
/// the rect.
pub fn layout(children: &[TreemapNode], x: u16, y: u16, w: u16, h: u16) -> Vec<TreemapCell> {
    let mut out = Vec::new();
    layout_into(children, x, y, w, h, 0, &mut out);
    out
}

fn layout_into(
    children: &[TreemapNode],
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    depth: u16,
    out: &mut Vec<TreemapCell>,
) {
    if w == 0 || h == 0 {
        return;
    }
    let items: Vec<&TreemapNode> = sorted_children(children);
    if items.is_empty() {
        return;
    }

    // Issue #527: a rect too small to draw at all gets folded into
    // one merged `…` cell per parent, sized by the combined total of
    // everything folded into it, and the parent is squarified again
    // with that merged entry standing in for all of them. Finding
    // F10: that second pass can itself still leave a survivor (or the
    // merged bucket's own cell) under 2×1 — rarer, but the same
    // problem one level removed — so this repeats, folding in
    // whatever is newly too small each time, until a pass leaves
    // nothing new to fold in. Bounded by `items.len()` iterations:
    // each one either converges, or strictly shrinks the survivor
    // count by at least one.
    let mut survivors: Vec<(&TreemapNode, u64)> = items.iter().map(|n| (*n, n.total)).collect();
    let mut merged_total: u64 = 0;

    let (weights, int_rects) = loop {
        let mut weights: Vec<(Option<&TreemapNode>, u64)> =
            survivors.iter().map(|(n, t)| (Some(*n), *t)).collect();
        if merged_total > 0 {
            weights.push((None, merged_total));
        }
        weights.sort_by(|(a_node, a_total), (b_node, b_total)| {
            b_total
                .cmp(a_total)
                .then_with(|| sort_key(*a_node).cmp(sort_key(*b_node)))
        });
        let weight_values: Vec<f64> = weights.iter().map(|(_, t)| *t as f64).collect();
        let rects = squarify(&weight_values, x as f64, y as f64, w as f64, h as f64);
        let int_rects: Vec<Rect> = rects.into_iter().map(round_rect).collect();

        let mut newly_too_small: u64 = 0;
        let mut next_survivors: Vec<(&TreemapNode, u64)> = Vec::new();
        for ((node, total), rect) in weights.iter().zip(int_rects.iter()) {
            if too_small(rect.w, rect.h) {
                // The merged bucket (`node: None`) itself coming up
                // too small contributes nothing further to fold in —
                // there is nothing smaller left than the bucket that
                // already holds everything too small to draw.
                if node.is_some() {
                    newly_too_small += total;
                }
            } else if let Some(n) = node {
                next_survivors.push((*n, *total));
            }
        }
        if newly_too_small == 0 {
            break (weights, int_rects);
        }
        survivors = next_survivors;
        merged_total += newly_too_small;
    };

    for ((node, total), rect) in weights.iter().zip(int_rects.iter()) {
        match node {
            Some(node) => emit_cell(node, *rect, depth, false, out),
            None => out.push(TreemapCell {
                label: "…".to_string(),
                path: None,
                rect: *rect,
                total: *total,
                status: None,
                has_children: false,
                merged: true,
                depth,
            }),
        }
    }
}

/// `None` sorts after every real node, so the merged `"…"` bucket
/// (built with no node of its own) is a stable, deterministic
/// tie-break partner for [`layout_into`]'s second squarify pass too.
fn sort_key(node: Option<&TreemapNode>) -> &str {
    node.map(|n| n.key.as_str()).unwrap_or("\u{10FFFF}")
}

fn emit_cell(node: &TreemapNode, rect: Rect, depth: u16, merged: bool, out: &mut Vec<TreemapCell>) {
    let can_recurse =
        !node.children.is_empty() && rect.w >= MIN_BORDER_RECT.0 && rect.h >= MIN_BORDER_RECT.1;
    out.push(TreemapCell {
        label: node.key.clone(),
        path: Some(node.path.clone()),
        rect,
        total: node.total,
        status: node.status,
        has_children: can_recurse,
        merged,
        depth,
    });
    if can_recurse {
        layout_into(
            &node.children,
            rect.x + 1,
            rect.y + 1,
            rect.w - 2,
            rect.h - 2,
            depth + 1,
            out,
        );
    }
}

/// Deterministic input order (issue #527): largest total first, ties
/// broken by key — never by children order or observation order.
fn sorted_children(children: &[TreemapNode]) -> Vec<&TreemapNode> {
    let mut items: Vec<&TreemapNode> = children.iter().filter(|n| n.total > 0).collect();
    items.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.key.cmp(&b.key)));
    items
}

/// The worst (farthest from 1.0) aspect ratio any rect in `row` would
/// get if the row (of total area `sum(row)`) were laid out along a
/// side of length `length` (Bruls et al.'s own `worst`).
fn worst_ratio(row: &[f64], length: f64) -> f64 {
    if row.is_empty() || length <= 0.0 {
        return f64::INFINITY;
    }
    let sum: f64 = row.iter().sum();
    let thickness = sum / length;
    if thickness <= 0.0 {
        return f64::INFINITY;
    }
    row.iter().fold(0.0_f64, |worst, &area| {
        let other_side = area / thickness;
        if other_side <= 0.0 {
            return f64::INFINITY;
        }
        let ratio = (thickness / other_side).max(other_side / thickness);
        worst.max(ratio)
    })
}

/// The squarified treemap algorithm itself, over normalised areas
/// (`areas` already scaled so `sum(areas) == w * h`, as this module's
/// one caller, [`layout_into`], always arranges by construction —
/// weight and area then coincide exactly, so `worst_ratio` can use
/// `areas` directly). `areas` must already be sorted descending
/// (`sorted_children`'s own order) — the algorithm's own row-growth
/// rule assumes it.
fn squarify(weights: &[f64], x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 || weights.is_empty() {
        return Vec::new();
    }
    let scale = (w * h) / total;
    let areas: Vec<f64> = weights.iter().map(|wt| wt * scale).collect();

    let mut out = Vec::with_capacity(areas.len());
    let mut areas = areas.as_slice();
    let mut x = x;
    let mut y = y;
    let mut w = w;
    let mut h = h;
    while !areas.is_empty() {
        if areas.len() == 1 {
            out.push((x, y, w, h));
            break;
        }
        let short = w.min(h);
        let mut row_end = 1usize;
        while row_end < areas.len() {
            let grown = worst_ratio(&areas[..=row_end], short);
            let current = worst_ratio(&areas[..row_end], short);
            if grown <= current {
                row_end += 1;
            } else {
                break;
            }
        }
        let row = &areas[..row_end];
        let row_sum: f64 = row.iter().sum();
        if w <= h {
            let thickness = row_sum / w;
            let mut cx = x;
            for &a in row {
                let cw = a / thickness;
                out.push((cx, y, cw, thickness));
                cx += cw;
            }
            y += thickness;
            h -= thickness;
        } else {
            let thickness = row_sum / h;
            let mut cy = y;
            for &a in row {
                let ch = a / thickness;
                out.push((x, cy, thickness, ch));
                cy += ch;
            }
            x += thickness;
            w -= thickness;
        }
        areas = &areas[row_end..];
    }
    out
}

/// Rounds a continuous rect to an integer grid without ever opening a
/// gap or an overlap with its neighbours (the module doc comment's
/// "every cell in a row shares the exact same boundary value"):
/// rounding the *boundary* (`x`, and `x + w`) rather than the width
/// itself is what makes that true, since two adjacent rects share the
/// exact same `f64` boundary value as input to `round()`.
fn round_rect((x, y, w, h): (f64, f64, f64, f64)) -> Rect {
    let left = x.round();
    let top = y.round();
    let right = (x + w).round();
    let bottom = (y + h).round();
    Rect {
        x: left.max(0.0) as u16,
        y: top.max(0.0) as u16,
        w: (right - left).max(0.0) as u16,
        h: (bottom - top).max(0.0) as u16,
    }
}

/// Builds the `tokens by key` tree (DESIGN.md §6.3) from flattened
/// state: issue #527's own "the root cell is `prime`" — a single
/// [`TreemapNode`] for `root_path` itself (normally `"prime"`),
/// carrying its own subtree's token total and recursing into its
/// children exactly the way any other container does ([`build_node`]
/// makes no distinction between the root and a nested container at
/// all). "Nothing has tokens yet" is an empty list, not a zero-area
/// root cell — the caller still calls [`layout`] on it either way.
pub fn build_tree(
    root_path: &str,
    flat: &BTreeMap<String, crate::model::NodeMeta>,
    statuses: &BTreeMap<String, crate::model::RowStatus>,
) -> Vec<TreemapNode> {
    build_node(root_path, flat, statuses).into_iter().collect()
}

fn build_node(
    path: &str,
    flat: &BTreeMap<String, crate::model::NodeMeta>,
    statuses: &BTreeMap<String, crate::model::RowStatus>,
) -> Option<TreemapNode> {
    let (sent, received) = crate::model::subtree_tokens(path, flat);
    let total = sent + received;
    if total == 0 {
        return None;
    }
    let key = path.rsplit('.').next().unwrap_or(path).to_string();
    let status = statuses.get(path).map(|s| s.kind);

    // `subtree_entries` bounds this to `path`'s own subtree instead
    // of scanning every path in the state (issue #527's redraw-cost
    // acceptance); checking only the last pushed entry (not `Vec::
    // contains`) keeps de-duplicating from becoming quadratic in the
    // child count too — safe because `subtree_entries`' own order is
    // sorted, so duplicates are always adjacent.
    let prefix = format!("{path}.");
    let mut direct_children: Vec<String> = Vec::new();
    for (p, _) in crate::model::subtree_entries(path, flat) {
        let Some(rest) = p.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let segment = rest.split('.').next().unwrap_or(rest);
        let child_path = format!("{path}.{segment}");
        if direct_children.last().map(String::as_str) != Some(child_path.as_str()) {
            direct_children.push(child_path);
        }
    }
    let children: Vec<TreemapNode> = direct_children
        .into_iter()
        .filter_map(|child_path| build_node(&child_path, flat, statuses))
        .collect();

    Some(TreemapNode {
        key,
        path: path.to_string(),
        total,
        status,
        children,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(key: &str, total: u64) -> TreemapNode {
        TreemapNode::leaf(key, total, Some(StatusKind::Done))
    }

    #[test]
    fn two_children_get_areas_proportional_to_their_totals() {
        let children = vec![leaf("a", 2000), leaf("b", 1000)];
        let cells = layout(&children, 0, 0, 30, 10);
        let a = cells.iter().find(|c| c.label == "a").unwrap();
        let b = cells.iter().find(|c| c.label == "b").unwrap();
        let area_a = a.rect.w as f64 * a.rect.h as f64;
        let area_b = b.rect.w as f64 * b.rect.h as f64;
        // 2:1 by weight; allow rounding slack on a 30x10 = 300-cell grid.
        let ratio = area_a / area_b;
        assert!((ratio - 2.0).abs() < 0.3, "ratio was {ratio}");
    }

    #[test]
    fn children_never_escape_the_parent_rect() {
        let children = vec![leaf("a", 5), leaf("b", 3), leaf("c", 1)];
        let cells = layout(&children, 2, 3, 20, 8);
        for cell in &cells {
            assert!(cell.rect.x >= 2, "{cell:?}");
            assert!(cell.rect.y >= 3, "{cell:?}");
            assert!(cell.rect.x + cell.rect.w <= 22, "{cell:?}");
            assert!(cell.rect.y + cell.rect.h <= 11, "{cell:?}");
        }
    }

    #[test]
    fn a_zero_token_key_gets_no_cell() {
        let children = vec![leaf("a", 10), leaf("zero", 0)];
        let cells = layout(&children, 0, 0, 20, 10);
        assert!(cells.iter().all(|c| c.label != "zero"));
        assert_eq!(cells.len(), 1);
    }

    #[test]
    fn many_tiny_children_merge_into_one_dots_cell() {
        // Nine 1-token leaves inside a big 100-token one, in a rect
        // narrow enough that each tiny leaf's own share rounds under
        // 2x1 -- they must all fold into exactly one "…" cell.
        let mut children = vec![leaf("big", 100)];
        for i in 0..9 {
            children.push(leaf(&format!("tiny{i}"), 1));
        }
        let cells = layout(&children, 0, 0, 12, 6);
        let merged: Vec<&TreemapCell> = cells.iter().filter(|c| c.merged).collect();
        assert_eq!(merged.len(), 1, "cells were {cells:#?}");
        assert_eq!(merged[0].label, "…");
        assert_eq!(merged[0].total, 9);
        assert!(cells.iter().any(|c| c.label == "big"));
    }

    #[test]
    fn the_same_input_always_produces_the_same_rectangles() {
        let children = vec![
            leaf("summarize", 1052),
            leaf("fetch", 400),
            leaf("draft", 90),
        ];
        // Finding F12: the same three nodes, given in a different
        // order, must still lay out identically -- `sorted_children`'s
        // own largest-total-first/tie-by-key order is what "the same
        // input" actually means here, not "the same `Vec` order",
        // which a call site's own observation order never guarantees.
        let shuffled = vec![
            leaf("draft", 90),
            leaf("summarize", 1052),
            leaf("fetch", 400),
        ];
        let first = layout(&children, 0, 0, 60, 20);
        let second = layout(&children, 0, 0, 60, 20);
        let from_shuffled = layout(&shuffled, 0, 0, 60, 20);
        assert_eq!(first, second);
        assert_eq!(first, from_shuffled);
    }

    #[test]
    fn a_container_lays_out_its_own_children_inside_a_border() {
        let child_a = leaf("fetch", 100);
        let child_b = leaf("summarize", 50);
        let container = TreemapNode {
            key: "loop".to_string(),
            path: "prime.loop".to_string(),
            total: 150,
            status: Some(StatusKind::Running),
            children: vec![child_a, child_b],
        };
        let cells = layout(&[container], 0, 0, 40, 20);
        let loop_cell = cells.iter().find(|c| c.label == "loop").unwrap();
        assert!(loop_cell.has_children);
        let fetch_cell = cells.iter().find(|c| c.label == "fetch").unwrap();
        // Inside the container's own border (one cell on every side).
        assert!(fetch_cell.rect.x > loop_cell.rect.x);
        assert!(fetch_cell.rect.y > loop_cell.rect.y);
        assert!(fetch_cell.rect.x + fetch_cell.rect.w < loop_cell.rect.x + loop_cell.rect.w);
        assert!(fetch_cell.rect.y + fetch_cell.rect.h < loop_cell.rect.y + loop_cell.rect.h);
    }

    #[test]
    fn build_tree_from_flat_state_omits_zero_token_subtrees() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fetch": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
            "summarize": {"value": "y", "meta": {"created_at": "t0", "completed_at": "t1", "adapter": "scripted", "tokens_sent_total": 10, "tokens_received_total": 20}}
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let tree = build_tree("prime", &flat, &statuses);
        // Issue #527: the root cell is `prime` itself, wrapping
        // whichever of its children actually have tokens.
        assert_eq!(tree.len(), 1);
        let root = &tree[0];
        assert_eq!(root.key, "prime");
        assert_eq!(root.total, 30);
        let keys: Vec<&str> = root.children.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, vec!["summarize"]);
    }

    #[test]
    fn build_tree_returns_empty_when_nothing_has_tokens_yet() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fetch": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let tree = build_tree("prime", &flat, &statuses);
        assert!(tree.is_empty());
    }
}
