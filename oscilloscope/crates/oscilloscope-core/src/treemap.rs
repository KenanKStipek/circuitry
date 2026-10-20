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
use crate::render::RowKind;

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
    /// The real, unboosted value under the tree's own [`Measure`] --
    /// what every figure a front end actually draws (a tile's own
    /// label, a container's title, a row's share) is computed from,
    /// and what a container's own total is: the sum of its
    /// children's real totals, never their [`Self::weight`]. A
    /// running leaf with nothing reported yet is `0` here, same as a
    /// finished one that reported nothing (though the latter is never
    /// built as a node at all).
    pub total: u64,
    /// [`layout`]'s own sizing input -- equal to `total` except for a
    /// leaf or container still sitting at nothing but a running
    /// placeholder ([`boost_running_placeholders`]), which needs a
    /// real share of its row to stay drawable even though it has
    /// nothing to show yet. Never read by anything that displays a
    /// number: only squarify's own area math, [`sorted_children`]'s
    /// own order, and the merge-pass "too small to draw" decisions in
    /// [`layout_into`] ever consult this field.
    pub weight: u64,
    pub status: Option<StatusKind>,
    /// A running leaf whose own [`Self::total`] came from `meta.live.
    /// tokens_sent` (issue #637), not the bare running placeholder --
    /// the one thing a front end's own tile label needs to know to
    /// say `live` instead of just the number, since by the time it
    /// draws, `total > 0` alone no longer tells a live-sized leaf
    /// apart from a container that already has a real, finished
    /// child. `false` for every other node, folded container included.
    pub live: bool,
    /// A running agent leaf's own `turn_tokens` (so far) and
    /// `tokens_cached_in`, straight from `meta.live` (issue #637's
    /// Cost section: "the per-turn strip" and "the cache bar") --
    /// `None` for every node [`Self::live`] is `false` for too.
    pub live_turns: Option<LiveTurnInfo>,
    pub children: Vec<TreemapNode>,
}

/// [`TreemapNode::live_turns`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LiveTurnInfo {
    pub turn_tokens: Vec<u64>,
    pub tokens_cached_in: Option<u64>,
}

impl TreemapNode {
    pub fn leaf(key: impl Into<String>, total: u64, status: Option<StatusKind>) -> Self {
        let key = key.into();
        TreemapNode {
            path: key.clone(),
            key,
            total,
            weight: total,
            status,
            live: false,
            live_turns: None,
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
    /// [`TreemapNode::live`], carried through to the drawn cell.
    pub live: bool,
    /// [`TreemapNode::live_turns`], carried through to the drawn cell.
    pub live_turns: Option<LiveTurnInfo>,
}

/// A rect too small to hold a border *and* at least one row/column of
/// interior for its children (issue #527's "less than 2×1 inside
/// their parent" governs leaf cells; a container additionally needs
/// the border itself, one cell on every side).
const MIN_BORDER_RECT: (u16, u16) = (3, 3);

/// A typical terminal character cell's own height-to-width ratio
/// (issue #632, DESIGN.md: "tiles are squarified for the terminal's
/// tall cells (about 2:1), so they look square on screen") --
/// `layout_into` stretches the y-axis by this factor before running
/// `squarify`'s own aspect-ratio comparisons, then divides every
/// resulting rect's `y`/`h` back down by it, so a tile that is
/// actually square in *character cells* (and so visibly tall and
/// narrow on screen) is not what "the worst aspect ratio" means any
/// more -- a visually square tile is.
const CELL_ASPECT: f64 = 2.0;

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
    // one merged `…` cell per parent, sized by the combined weight of
    // everything folded into it, and the parent is squarified again
    // with that merged entry standing in for all of them. That
    // second pass can itself still leave a survivor (or the merged
    // bucket's own cell) under 2×1 — rarer, but the same problem one
    // level removed — so this repeats, folding in whatever is newly
    // too small each time, until a pass leaves nothing new to fold
    // in. Bounded by `items.len()` iterations: each one either
    // converges, or strictly shrinks the survivor count by at least
    // one. The merged bucket's own displayed number is the real
    // *total* of everything folded into it, never the sizing
    // *weight* that decided it needed folding in the first place --
    // the same real/weight split every other cell keeps.
    let mut survivors: Vec<&TreemapNode> = items;
    let mut merged_weight: u64 = 0;
    let mut merged_total: u64 = 0;

    let (weights, int_rects) = loop {
        let mut weights: Vec<(Option<&TreemapNode>, u64)> =
            survivors.iter().map(|n| (Some(*n), n.weight)).collect();
        if merged_weight > 0 {
            weights.push((None, merged_weight));
        }
        weights.sort_by(|(a_node, a_weight), (b_node, b_weight)| {
            b_weight
                .cmp(a_weight)
                .then_with(|| sort_key(*a_node).cmp(sort_key(*b_node)))
        });
        let weight_values: Vec<f64> = weights.iter().map(|(_, w)| *w as f64).collect();
        // Issue #632: the squarify decisions below (`worst_ratio`,
        // which side a row grows along) are run in a "visual" space
        // where the y-axis is stretched by `CELL_ASPECT` first --
        // without this, "worst aspect ratio" means closest to a
        // *character-cell* square, which on a terminal (whose cells
        // are themselves about `CELL_ASPECT` times taller than wide)
        // draws tiles that look visibly tall and narrow rather than
        // square. Every output rect is converted straight back to
        // real character-cell units (`ry / CELL_ASPECT`, `rh /
        // CELL_ASPECT`) before rounding, so nothing downstream of
        // this ever sees the stretched space at all.
        let rects = squarify(
            &weight_values,
            x as f64,
            y as f64 * CELL_ASPECT,
            w as f64,
            h as f64 * CELL_ASPECT,
        );
        let int_rects: Vec<Rect> = rects
            .into_iter()
            .map(|(rx, ry, rw, rh)| round_rect((rx, ry / CELL_ASPECT, rw, rh / CELL_ASPECT)))
            .collect();

        let mut newly_too_small_weight: u64 = 0;
        let mut newly_too_small_total: u64 = 0;
        let mut next_survivors: Vec<&TreemapNode> = Vec::new();
        for ((node, _weight), rect) in weights.iter().zip(int_rects.iter()) {
            if too_small(rect.w, rect.h) {
                // The merged bucket (`node: None`) itself coming up
                // too small contributes nothing further to fold in —
                // there is nothing smaller left than the bucket that
                // already holds everything too small to draw.
                if let Some(n) = node {
                    newly_too_small_weight += n.weight;
                    newly_too_small_total += n.total;
                }
            } else if let Some(n) = node {
                next_survivors.push(*n);
            }
        }
        if newly_too_small_weight == 0 {
            break (weights, int_rects);
        }
        survivors = next_survivors;
        merged_weight += newly_too_small_weight;
        merged_total += newly_too_small_total;
    };

    for ((node, _weight), rect) in weights.iter().zip(int_rects.iter()) {
        match node {
            Some(node) => emit_cell(node, *rect, depth, false, out),
            None => out.push(TreemapCell {
                label: "…".to_string(),
                path: None,
                rect: *rect,
                total: merged_total,
                status: None,
                has_children: false,
                merged: true,
                depth,
                live: false,
                live_turns: None,
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
        live: node.live,
        live_turns: node.live_turns.clone(),
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

/// Deterministic input order (issue #527): largest *weight* first
/// (the squarify-sizing field -- a running store's own boosted
/// placeholder weight, not its real, possibly-zero, total, is what
/// decides its place here too), ties broken by key — never by
/// children order or observation order. `pub` so a front end's own
/// selection order (issue #632: "↑/↓ select tiles in size order")
/// can match exactly what [`layout`] itself lays tiles out in, rather
/// than keeping its own, possibly diverging, copy of this rule.
pub fn sorted_children(children: &[TreemapNode]) -> Vec<&TreemapNode> {
    let mut items: Vec<&TreemapNode> = children.iter().filter(|n| n.weight > 0).collect();
    items.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.key.cmp(&b.key)));
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

/// The Cost screen's own treemap measure (issue #632): `Total`
/// (tokens in + out) is the default; `i`/`o`/`$` switch to input
/// tokens, output tokens or cost, and the same key again returns to
/// `Total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Total,
    In,
    Out,
    Cost,
}

impl Measure {
    pub fn label(self) -> &'static str {
        match self {
            Measure::Total => "tokens",
            Measure::In => "input tokens",
            Measure::Out => "output tokens",
            Measure::Cost => "cost",
        }
    }

    /// `i`/`o`/`$` pick a measure; the same key again returns to
    /// [`Measure::Total`] (issue #632's own toggle rule). `None` for
    /// any other key -- `t` is never handled here, it stays the
    /// theme-cycle key.
    pub fn from_key(current: Measure, key: char) -> Option<Measure> {
        let target = match key {
            'i' => Measure::In,
            'o' => Measure::Out,
            '$' => Measure::Cost,
            _ => return None,
        };
        Some(if current == target {
            Measure::Total
        } else {
            target
        })
    }
}

/// Scales a dollar figure into the same integer weight
/// [`squarify`]'s geometry already uses for token counts -- a cell's
/// own `total` is in micro-dollars under [`Measure::Cost`], never a
/// raw cent or dollar count, so two stores a cent apart still order
/// (and size) correctly. [`display_cost_usd`] is the inverse, for a
/// front end showing the real figure back.
const COST_SCALE: f64 = 1_000_000.0;

pub fn display_cost_usd(total: u64) -> f64 {
    total as f64 / COST_SCALE
}

/// A nominal, non-zero weight for a leaf that is still running and
/// has not reported anything for the chosen measure yet (DESIGN.md:
/// "a tile for a step that is still running ... reads `tokens when
/// done`") -- real geometry still needs *some* positive weight to
/// give it a cell at all ([`squarify`]'s own "a key with total == 0
/// must not be passed in"); a front end never shows this number,
/// only the fixed "tokens when done" text, gated on [`TreemapCell`]'s
/// own `status`/`has_children` (`Some(StatusKind::Running)` and no
/// children at all).
const RUNNING_PLACEHOLDER_WEIGHT: u64 = 1;

fn own_measure_value(node: &crate::model::NodeMeta, measure: Measure) -> u64 {
    match measure {
        Measure::Total => {
            let (sent, received) = crate::model::node_tokens(&node.meta);
            sent + received
        }
        Measure::In => crate::model::node_tokens(&node.meta).0,
        Measure::Out => crate::model::node_tokens(&node.meta).1,
        Measure::Cost => node
            .meta
            .get("cost_usd")
            .and_then(serde_json::Value::as_f64)
            .map(|c| (c.max(0.0) * COST_SCALE).round() as u64)
            .unwrap_or(0),
    }
}

/// Whether `meta` belongs to a kind of leaf that could ever report a
/// token or cost figure at all (a prompt/reflector's own `adapter`/
/// `prompt_sent`, or an agent tool's `provider`) -- the same shapes
/// `screens::is_agent_row` and DESIGN.md §1.2 check elsewhere. A
/// plain tool never gets a cell here, running or not: it is not a
/// store this screen tracks at all, so it must not count toward the
/// "stores without <measure>" footer either, and a running one must
/// not get [`RUNNING_PLACEHOLDER_WEIGHT`]'s own "tokens when done"
/// treatment -- it will never report one.
fn is_token_bearing_kind(meta: &serde_json::Value) -> bool {
    meta.get("adapter").is_some()
        || meta.get("prompt_sent").is_some()
        || meta
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|p| p.eq_ignore_ascii_case("agent"))
}

/// Every direct child segment of `path` actually present somewhere
/// under it in `flat` (issue #527's own redraw-cost bound:
/// `subtree_entries` stays `O(log n + matches)`, never a full scan).
fn direct_children_of(path: &str, flat: &BTreeMap<String, crate::model::NodeMeta>) -> Vec<String> {
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
    direct_children
}

/// Builds the Cost screen's own by-store tree for one [`Measure`]
/// (issue #632), replacing the old `tokens by key` treemap screen 2
/// had (always rooted at one `prime` cell): returns `root_path`'s own
/// *children*, each already chain-folded ([`fold_single_child`]),
/// as the top-level tiles -- there is no single wrapping root cell
/// any more, since the mockup's own top-level tiles are the state
/// stores themselves (`plan › agent`, `implement › chunk 1 › agent`,
/// ...), not one big box around all of them. The second return value
/// is how many *finished* stores were left out for having no value
/// under this measure at all (DESIGN.md's `3 stores without cost`
/// footer) -- a store still running never counts there, since it
/// always gets a cell of its own via [`RUNNING_PLACEHOLDER_WEIGHT`].
/// `kinds` is a path's own [`RowKind`] where one is known -- the
/// same map [`fold_single_child`] needs to apply the orchestration
/// pane's own breadcrumb-eligibility rule to a store, rather than a
/// narrower copy of it.
pub fn build_tree_for_measure(
    root_path: &str,
    flat: &BTreeMap<String, crate::model::NodeMeta>,
    statuses: &BTreeMap<String, crate::model::RowStatus>,
    kinds: &BTreeMap<String, RowKind>,
    measure: Measure,
) -> (Vec<TreemapNode>, u64) {
    let mut excluded = 0u64;
    let root = build_node_for_measure(root_path, flat, statuses, kinds, measure, &mut excluded);
    let mut children = root.map(|r| r.children).unwrap_or_default();
    // The root never gets a tile of its own: a `check` leaf built
    // from its own meta (the `while: mode: model` case just below)
    // would otherwise surface right alongside the real top-level
    // stores -- it is never one, no matter what its own meta reports
    // (every other container's own `check` leaf stays, nested inside
    // that container's own tile instead of at the top level).
    children.retain(|c| c.path != root_path);
    (children, excluded)
}

fn build_node_for_measure(
    path: &str,
    flat: &BTreeMap<String, crate::model::NodeMeta>,
    statuses: &BTreeMap<String, crate::model::RowStatus>,
    kinds: &BTreeMap<String, RowKind>,
    measure: Measure,
    excluded: &mut u64,
) -> Option<TreemapNode> {
    let status = statuses.get(path).map(|s| s.kind);
    let direct_children = direct_children_of(path, flat);
    if direct_children.is_empty() {
        return leaf_node_for_measure(path, flat, status, measure, excluded);
    }
    let mut children: Vec<TreemapNode> = direct_children
        .into_iter()
        .filter_map(|child_path| {
            build_node_for_measure(&child_path, flat, statuses, kinds, measure, excluded)
        })
        .map(|child| fold_single_child(child, kinds))
        .collect();
    // A `while: mode: model` loop writes its own `tokens_sent_total`/
    // `tokens_received_total` (and, for `Measure::Cost`, its own
    // `cost_usd`) onto *this* container's meta, every pass, never a
    // null placeholder here (`docs/orchestration-reference.md`'s own
    // `while.template` row) -- a container that *already reports* a
    // nonzero value of its own is a store in exactly the same sense
    // its children are, so it gets a leaf tile of its own alongside
    // them rather than vanishing into an unexplained gap between this
    // screen's total and `runtime.last_run.totals`. Unlike a true
    // leaf, a container merely still running with nothing of its own
    // reported *yet* gets no placeholder tile at all --
    // `container_own_value_node` never applies [`RUNNING_PLACEHOLDER_
    // WEIGHT`] the way [`leaf_node_for_measure`] does, since nearly
    // every container's own meta carries an `adapter` key (the root's
    // included) without ever meaning to report its own measure the
    // way a `while: mode: model` loop deliberately does.
    if let Some(own_leaf) = container_own_value_node(path, flat, status, measure) {
        children.push(TreemapNode {
            key: "check".to_string(),
            ..own_leaf
        });
    }
    boost_running_placeholders(&mut children);
    let total: u64 = children.iter().map(|c| c.total).sum();
    let weight: u64 = children.iter().map(|c| c.weight).sum();
    if weight == 0 {
        return None;
    }
    let key = path.rsplit('.').next().unwrap_or(path).to_string();
    Some(TreemapNode {
        key,
        path: path.to_string(),
        total,
        weight,
        status,
        live: false,
        live_turns: None,
        children,
    })
}

/// The leaf-building half of [`build_node_for_measure`] -- `excluded`
/// counts a *finished* leaf that reported nothing under `measure` at
/// all (DESIGN.md's `3 stores without cost` footer).
fn leaf_node_for_measure(
    path: &str,
    flat: &BTreeMap<String, crate::model::NodeMeta>,
    status: Option<StatusKind>,
    measure: Measure,
    excluded: &mut u64,
) -> Option<TreemapNode> {
    let node = flat.get(path)?;
    let running = node.completed_at.is_none();
    let key = path.rsplit('.').next().unwrap_or(path).to_string();
    if running {
        // A running leaf has not reported any tokens/cost of its own
        // yet, so (unlike the finished branch below) there is
        // nothing in its own numbers to filter a plain tool out by --
        // only its *kind* can: without this, a running plain tool
        // would get a placeholder cell too, though it will never
        // report one.
        if !is_token_bearing_kind(&node.meta) {
            return None;
        }
        // `total` is the real value (zero, almost always, since a
        // running leaf has nothing to report yet) -- only `weight`
        // gets the placeholder floor, so a front end drawing "tokens
        // when done" from a leaf whose `total` is honestly zero never
        // has to subtract this floor back out again.
        let total = own_measure_value(node, measure);
        // Issue #637: a running agent's own live token count
        // (`meta.live.tokens_sent`, the one figure the issue's own
        // Cost section names) replaces the placeholder floor for
        // `Measure::Total`/`Measure::In` -- `tokens_sent` is "input
        // tokens" either way, so it is the right figure under both.
        // `Out`/`Cost` keep the placeholder: pi does report `tokens_
        // received`/`cost_usd` live too (`docs/plugins/agent.md`),
        // but growing the tile from them is left for a later issue.
        let live_tokens = matches!(measure, Measure::Total | Measure::In)
            .then(|| node.live.as_ref().and_then(|live| live.tokens_sent))
            .flatten();
        let total = live_tokens.map_or(total, |live_tokens| total.max(live_tokens));
        let weight = total.max(RUNNING_PLACEHOLDER_WEIGHT);
        // The per-turn strip and the cache bar (issue #637's Cost
        // section) ride along with the same live tile, straight from
        // `meta.live` -- `turn_tokens`/`tokens_cached_in` have no
        // placeholder of their own to replace, so they're carried
        // whenever the tile itself is live, not gated separately.
        let live_turns = live_tokens
            .is_some()
            .then_some(node.live.as_ref())
            .flatten()
            .map(|live| LiveTurnInfo {
                turn_tokens: live.turn_tokens.clone(),
                tokens_cached_in: live.tokens_cached_in,
            });
        return Some(TreemapNode {
            key,
            path: path.to_string(),
            total,
            weight,
            status,
            live: live_tokens.is_some(),
            live_turns,
            children: Vec::new(),
        });
    }
    let (sent, received) = crate::model::node_tokens(&node.meta);
    let has_cost = node.meta.get("cost_usd").is_some();
    if sent == 0 && received == 0 && !has_cost {
        // Never reported anything at all under any measure -- not
        // a store this screen tracks, so not counted either.
        return None;
    }
    let total = own_measure_value(node, measure);
    if total == 0 {
        *excluded += 1;
        return None;
    }
    Some(TreemapNode {
        key,
        path: path.to_string(),
        total,
        weight: total,
        status,
        live: false,
        live_turns: None,
        children: Vec::new(),
    })
}

/// A container's own leaf tile (the `while: mode: model` case,
/// [`build_node_for_measure`]'s own doc comment) -- unlike
/// [`leaf_node_for_measure`], never a running placeholder: a
/// container that is merely still running, with nothing of its own
/// reported yet, gets no tile of its own at all. Only a container
/// that has *already* reported a real, nonzero value under `measure`
/// qualifies -- the same bar a finished leaf clears in `leaf_node_
/// for_measure`, applied here regardless of whether this container
/// itself has already finished (a `while: mode: model` loop writes
/// its own totals as it goes, one pass at a time, well before the
/// loop itself is done).
fn container_own_value_node(
    path: &str,
    flat: &BTreeMap<String, crate::model::NodeMeta>,
    status: Option<StatusKind>,
    measure: Measure,
) -> Option<TreemapNode> {
    let node = flat.get(path)?;
    let (sent, received) = crate::model::node_tokens(&node.meta);
    let has_cost = node.meta.get("cost_usd").is_some();
    if sent == 0 && received == 0 && !has_cost {
        return None;
    }
    let total = own_measure_value(node, measure);
    if total == 0 {
        return None;
    }
    let key = path.rsplit('.').next().unwrap_or(path).to_string();
    Some(TreemapNode {
        key,
        path: path.to_string(),
        total,
        weight: total,
        status,
        live: false,
        live_turns: None,
        children: Vec::new(),
    })
}

/// A median of `totals` (ties rounded down to the lower of the two
/// middle values, same as a plain sorted index lookup) -- `None` for
/// `&[]`.
fn median_total(totals: &[u64]) -> Option<u64> {
    if totals.is_empty() {
        return None;
    }
    let mut sorted = totals.to_vec();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
}

/// A running leaf's own weight, [`RUNNING_PLACEHOLDER_WEIGHT`], is
/// too small to survive squarify at all next to siblings with real
/// token counts in the thousands or millions -- it rounds to under
/// 2×1 and silently folds into the `…` merged bucket, so DESIGN.md's
/// own "a tile for a step that is still running ... reads `tokens
/// when done`" never actually shows, and the same happens one level
/// up to a container whose entire subtree is still just that one
/// placeholder (a loop with a single running pass, say) -- its own
/// `weight` is the placeholder value unchanged, bubbled straight up
/// through however many containers wrap it (each container's own
/// `weight` is the sum of its children's, same as `total` is the sum
/// of their real totals), however far from its own real siblings
/// that puts it. This boosts *any* child still sitting at the bare
/// placeholder weight -- leaf or container, whatever its own status
/// -- to its siblings' own median *weight* above that floor, which
/// [`layout`] then treats the same as any other real weight: a
/// visible share of the row, never a number a front end actually
/// shows (gated on `status`/`has_children`, and `total > 0`, the same
/// as every other running leaf -- `total` itself is never touched
/// here, only `weight`). A sibling group with nothing above the floor
/// either (every child still running, or genuinely all at 1 token)
/// gets no boost at all -- there is nothing realer to measure against
/// yet.
fn boost_running_placeholders(children: &mut [TreemapNode]) {
    let real_weights: Vec<u64> = children
        .iter()
        .map(|c| c.weight)
        .filter(|&w| w > RUNNING_PLACEHOLDER_WEIGHT)
        .collect();
    let Some(median) = median_total(&real_weights) else {
        return;
    };
    for child in children.iter_mut() {
        if child.weight <= RUNNING_PLACEHOLDER_WEIGHT {
            child.weight = median;
        }
    }
}

/// Issue #632's own chain folding, by the exact same rule
/// `fold::fold_node`'s breadcrumbs use for the orchestration pane's
/// rows: a node with exactly one child folds with it into one tile,
/// `outer › inner`, only once both sides are `fold::breadcrumb_
/// eligible` (neither is the document root, a loop container or a
/// quick-steps summary, and neither's own status is still
/// `Pending`) and their statuses actually agree -- a `use` whose
/// `expect` fails after its one child succeeded must not show `✓`,
/// and a still-running container whose only child has already
/// finished must not show done. `kinds` supplies each side's own
/// `RowKind` (`None` for a path with no row of its own, which is
/// never eligible either) since a [`TreemapNode`] does not carry one
/// itself. The fold keeps the child's own path (so zoom and
/// selection still target it) and status. Applied to each of a
/// container's own children as they're collected
/// ([`build_node_for_measure`]), never to the node the caller itself
/// just built -- that's what keeps the top root (`build_tree_for_
/// measure`'s own `root_path`) from ever folding its single child
/// away into itself: nothing above the top call ever folds it.
fn fold_single_child(node: TreemapNode, kinds: &BTreeMap<String, RowKind>) -> TreemapNode {
    if node.children.len() == 1
        && store_breadcrumb_eligible(&node, kinds)
        && store_breadcrumb_eligible(&node.children[0], kinds)
        && node.status.is_some()
        && node.status == node.children[0].status
    {
        let child = node.children.into_iter().next().expect("checked len == 1");
        return TreemapNode {
            key: format!("{} \u{203a} {}", node.key, child.key),
            path: child.path,
            total: node.total,
            weight: node.weight,
            status: child.status,
            live: child.live,
            live_turns: child.live_turns.clone(),
            children: child.children,
        };
    }
    node
}

/// `fold::kind_breadcrumb_eligible`, for a store rather than a row --
/// `kinds.get(path)` is `None` for a store with no row of its own at
/// all (e.g. `fold_single_child`'s own synthetic `"check"` leaf,
/// which shares its container's path and so is never itself a
/// distinct row), which is never eligible either, same as a known
/// but still-`Pending` one.
fn store_breadcrumb_eligible(node: &TreemapNode, kinds: &BTreeMap<String, RowKind>) -> bool {
    let Some(status) = node.status else {
        return false;
    };
    let Some(kind) = kinds.get(&node.path).copied() else {
        return false;
    };
    crate::fold::kind_breadcrumb_eligible(kind, status)
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
    fn tiles_are_squarified_for_the_terminals_tall_cells_not_square_characters() {
        // Two equal-weight leaves in a 10x8 character-cell rect: a
        // *character*-square split would place them side by side (two
        // 5x8 columns, each visually 5 wide but 16 "tall" once the
        // terminal's own ~2:1 cell aspect is accounted for -- about as
        // far from square as two tiles can be). The aspect-corrected
        // split instead stacks them (two 10x4 rows, each a visually
        // near-square 10x8), which this pins by checking the two
        // cells share an `x`, not a `y`.
        let children = vec![leaf("a", 1), leaf("b", 1)];
        let cells = layout(&children, 0, 0, 10, 8);
        assert_eq!(cells.len(), 2);
        let a = cells.iter().find(|c| c.label == "a").unwrap();
        let b = cells.iter().find(|c| c.label == "b").unwrap();
        assert_eq!(a.rect.x, b.rect.x, "cells were {cells:#?}");
        assert_ne!(a.rect.y, b.rect.y, "cells were {cells:#?}");
    }

    /// Issue #632: every tile big enough for its own rounding error to
    /// matter (at least 4x2 character cells) stays within this bound,
    /// on its *cell-corrected* aspect ratio (`width / (height *
    /// CELL_ASPECT)`, the same space `layout_into`'s own squarify
    /// comparisons run in) -- never the raw character-cell ratio,
    /// which `tiles_are_squarified_for_the_terminals_tall_cells_not_
    /// square_characters` above already covers for the 1:1 case.
    /// `squarify`'s own row-growth rule (`worst_ratio`) only ever
    /// grows a row while doing so does not make its worst ratio
    /// *worse*, so within one row the ratio is already well-behaved;
    /// what this bound actually has to absorb is the last, often
    /// oddly-shaped row of a long tail once integer rounding has its
    /// say. 4 is not a theoretical ceiling (squarify gives no such
    /// constant for an arbitrary weight sequence) -- it is the worst
    /// this module's own distributions below ever produced across
    /// every pane size tried while developing this test, including
    /// several deliberately more adversarial ones no longer kept here
    /// (a 5-way long tail at 60x18 hit exactly 4.0); it is asserted as
    /// a regression bound; a real failure here is worth a fresh look,
    /// not a reflexive bump.
    const ASPECT_BOUND: f64 = 4.0;

    fn weights_equal(n: usize) -> Vec<TreemapNode> {
        (0..n).map(|i| leaf(&format!("k{i}"), 100)).collect()
    }

    /// One large store (70% of the total) alongside several
    /// meaningfully-sized ones, not a handful of negligible ones --
    /// the latter would simply fold into the merged `…` bucket
    /// (`many_tiny_children_merge_into_one_dots_cell` above already
    /// covers that), never exercising squarify's own row-growth
    /// aspect-ratio trade-off at a realistic size.
    fn weights_one_dominant(n: usize) -> Vec<TreemapNode> {
        let mut v = vec![leaf("big", 7_000)];
        v.extend((0..n).map(|i| leaf(&format!("k{i}"), 600)));
        v
    }

    /// One large store plus a tail of `n` small ones whose own
    /// weights decay (`1000 / (i + 2)`), so most of the tail survives
    /// as its own real tile rather than folding into the merged `…`
    /// bucket -- the regime that actually stresses the last row's own
    /// aspect ratio once rounding lands unevenly across many small
    /// tiles.
    fn weights_long_tail(n: usize) -> Vec<TreemapNode> {
        let mut v = vec![leaf("big", 4_000)];
        for i in 0..n {
            v.push(leaf(&format!("k{i}"), 1000 / (i as u64 + 2)));
        }
        v
    }

    /// Whether two character-cell rects overlap: a closed x-range and
    /// a closed y-range both have to overlap at once.
    fn rects_overlap(a: Rect, b: Rect) -> bool {
        let a_x1 = a.x + a.w;
        let a_y1 = a.y + a.h;
        let b_x1 = b.x + b.w;
        let b_y1 = b.y + b.h;
        a.x < b_x1 && b.x < a_x1 && a.y < b_y1 && b.y < a_y1
    }

    #[test]
    fn tile_aspect_ratios_stay_within_bound_across_distributions_and_pane_sizes() {
        let dists: [(&str, Vec<TreemapNode>); 3] = [
            ("equal", weights_equal(8)),
            ("one dominant", weights_one_dominant(5)),
            ("long tail of 20", weights_long_tail(20)),
        ];
        let panes = [(120u16, 40u16), (80u16, 24u16), (60u16, 18u16)];

        for (dist_name, children) in &dists {
            for (w, h) in panes {
                let cells = layout(children, 0, 0, w, h);
                assert!(
                    !cells.is_empty(),
                    "{dist_name} at {w}x{h} produced no cells"
                );

                // No two tiles overlap, and together they cover the
                // whole pane exactly (every cell here is a leaf -- no
                // container's own child rect nests inside another
                // cell's area) -- squarify's own boundary-sharing
                // construction (this module's doc comment) is what
                // makes both true at once, not merely "close".
                for (i, a) in cells.iter().enumerate() {
                    for b in &cells[i + 1..] {
                        assert!(
                            !rects_overlap(a.rect, b.rect),
                            "{dist_name} at {w}x{h}: {a:?} overlaps {b:?}"
                        );
                    }
                }
                let covered: u64 = cells
                    .iter()
                    .map(|c| c.rect.w as u64 * c.rect.h as u64)
                    .sum();
                assert_eq!(
                    covered,
                    w as u64 * h as u64,
                    "{dist_name} at {w}x{h}: tiles cover {covered} of {} cells",
                    w as u64 * h as u64
                );

                for cell in &cells {
                    if cell.rect.w < 4 || cell.rect.h < 2 {
                        continue;
                    }
                    let ratio = cell.rect.w as f64 / (cell.rect.h as f64 * CELL_ASPECT);
                    let ratio = ratio.max(1.0 / ratio);
                    assert!(
                        ratio <= ASPECT_BOUND,
                        "{dist_name} at {w}x{h}: {cell:?} has cell-corrected aspect ratio {ratio}, over the {ASPECT_BOUND} bound"
                    );
                }
            }
        }
    }

    #[test]
    fn a_container_lays_out_its_own_children_inside_a_border() {
        let child_a = leaf("fetch", 100);
        let child_b = leaf("summarize", 50);
        let container = TreemapNode {
            key: "loop".to_string(),
            path: "prime.loop".to_string(),
            total: 150,
            weight: 150,
            status: Some(StatusKind::Running),
            live: false,
            live_turns: None,
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
    fn build_tree_for_measure_omits_zero_token_subtrees_and_has_no_wrapping_root_cell() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fetch": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
            "summarize": {"value": "y", "meta": {"created_at": "t0", "completed_at": "t1", "adapter": "scripted", "tokens_sent_total": 10, "tokens_received_total": 20}}
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, excluded) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        // Issue #632: no single `prime` wrapping cell any more -- the
        // top-level tiles are `prime`'s own children directly.
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "summarize");
        assert_eq!(top[0].total, 30);
        assert_eq!(
            excluded, 0,
            "a plain tool with no tokens is never a store at all"
        );
    }

    #[test]
    fn build_tree_for_measure_returns_empty_when_nothing_has_tokens_yet() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fetch": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, excluded) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert!(top.is_empty());
        assert_eq!(excluded, 0);
    }

    fn prompt_node(
        sent: u64,
        received: u64,
        cost: Option<f64>,
        running: bool,
    ) -> serde_json::Value {
        use serde_json::json;
        let mut meta = json!({
            "created_at": "t0",
            "completed_at": if running { serde_json::Value::Null } else { serde_json::Value::String("t1".to_string()) },
            "adapter": "scripted",
            "tokens_sent_total": sent,
            "tokens_received_total": received,
        });
        if let Some(cost) = cost {
            meta["cost_usd"] = json!(cost);
        }
        json!({"value": "y", "meta": meta})
    }

    #[test]
    fn measure_switching_reads_in_out_and_cost_separately() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "plan": prompt_node(100, 50, Some(1.5), false),
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (total, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        let (input, _) = build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::In);
        let (output, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Out);
        let (cost, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Cost);
        assert_eq!(total[0].total, 150);
        assert_eq!(input[0].total, 100);
        assert_eq!(output[0].total, 50);
        assert_eq!(display_cost_usd(cost[0].total), 1.5);
    }

    #[test]
    fn measure_from_key_toggles_back_to_total_on_the_same_key() {
        assert_eq!(Measure::from_key(Measure::Total, 'i'), Some(Measure::In));
        assert_eq!(Measure::from_key(Measure::In, 'i'), Some(Measure::Total));
        assert_eq!(Measure::from_key(Measure::Total, 'o'), Some(Measure::Out));
        assert_eq!(Measure::from_key(Measure::Out, 'o'), Some(Measure::Total));
        assert_eq!(Measure::from_key(Measure::Total, '$'), Some(Measure::Cost));
        assert_eq!(Measure::from_key(Measure::Cost, '$'), Some(Measure::Total));
        // Switching straight from one measure to a different one (not
        // the same key twice) just switches, it does not bounce
        // through `Total`.
        assert_eq!(Measure::from_key(Measure::In, 'o'), Some(Measure::Out));
        assert_eq!(Measure::from_key(Measure::Total, 't'), None);
    }

    #[test]
    fn a_single_child_chain_folds_into_one_breadcrumb_tile() {
        use crate::model::flatten_state;
        use serde_json::json;

        // `plan` wraps a lone `agent` child with the same (done)
        // status -- the chain must fold into one `plan › agent` tile
        // carrying `agent`'s own path.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "plan": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"},
                "agent": prompt_node(100, 50, None, false)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = [
            ("prime.plan".to_string(), RowKind::Use),
            ("prime.plan.agent".to_string(), RowKind::Prompt),
        ]
        .into_iter()
        .collect();
        statuses.insert(
            "prime.plan".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
        );
        statuses.insert(
            "prime.plan.agent".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
        );
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "plan \u{203a} agent");
        assert_eq!(top[0].path, "prime.plan.agent");
        assert!(top[0].children.is_empty());
    }

    #[test]
    fn a_chain_with_disagreeing_statuses_does_not_fold() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "plan": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                "agent": prompt_node(100, 50, None, false)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        statuses.insert(
            "prime.plan".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        statuses.insert(
            "prime.plan.agent".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
        );
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "plan");
        assert_eq!(top[0].children.len(), 1);
        assert_eq!(top[0].children[0].key, "agent");
    }

    #[test]
    fn a_running_prompt_gets_a_placeholder_cell_instead_of_vanishing() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "agent": prompt_node(0, 0, None, true),
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        statuses.insert(
            "prime.agent".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let (top, excluded) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].status, Some(StatusKind::Running));
        assert!(top[0].weight > 0, "a running store still gets a cell");
        assert_eq!(
            top[0].total, 0,
            "nothing reported yet is honestly zero, not a boosted number"
        );
        assert_eq!(excluded, 0, "a running store is never counted as excluded");
    }

    /// Issue #637: a running agent's own tile sizes itself from
    /// `meta.live.tokens_sent`, not the bare placeholder floor -- the
    /// one figure `docs/plugins/agent.md`'s live summary reports
    /// continuously.
    #[test]
    fn a_running_agents_tile_sizes_from_its_own_live_tokens_sent() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fix": {"value": null, "meta": {
                "created_at": "t0",
                "completed_at": serde_json::Value::Null,
                "provider": "agent",
                "live": {"tokens_sent": 2_100_000},
            }},
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        statuses.insert(
            "prime.fix".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let (top, excluded) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].total, 2_100_000, "sizes from the live figure, not 0");
        assert_eq!(excluded, 0);

        // `Measure::Out` has no live equivalent (`docs/plugins/
        // agent.md`): the running tile stays at the placeholder,
        // honestly reporting nothing yet, same as before this issue.
        let (out_top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Out);
        assert_eq!(out_top[0].total, 0);
    }

    #[test]
    fn a_running_plain_tool_never_gets_a_cell() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "build": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}},
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        statuses.insert(
            "prime.build".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let (top, excluded) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert!(top.is_empty());
        assert_eq!(excluded, 0);
    }

    #[test]
    fn a_finished_store_without_the_chosen_measure_is_excluded_and_counted() {
        use crate::model::flatten_state;
        use serde_json::json;

        // `plan` has tokens (a store under `Total`) but never reported
        // a cost -- excluded, and counted, under `Measure::Cost` only.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "plan": prompt_node(100, 50, None, false),
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top_total, excluded_total) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top_total.len(), 1);
        assert_eq!(excluded_total, 0);
        let (top_cost, excluded_cost) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Cost);
        assert!(top_cost.is_empty());
        assert_eq!(excluded_cost, 1);
    }

    #[test]
    fn a_running_leaf_next_to_large_finished_siblings_still_gets_a_drawable_cell() {
        use crate::model::flatten_state;
        use serde_json::json;

        // `big` and `bigger` dwarf a running leaf's own placeholder
        // weight of 1 by six orders of magnitude -- without boosting,
        // the running leaf's own share of a typical pane rounds under
        // 2x1 and silently folds into the merged "..." bucket, so
        // "tokens when done" never actually appears anywhere.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "big": prompt_node(900_000, 400_000, None, false),
            "bigger": prompt_node(1_300_000, 600_000, None, false),
            "fan": {"value": null, "meta": {"completed_at": null},
                "nap": prompt_node(0, 0, None, true)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        statuses.insert(
            "prime.fan.nap".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        let nap = top
            .iter()
            .flat_map(|n| n.children.iter().chain(std::iter::once(n)))
            .find(|n| n.path == "prime.fan.nap")
            .expect("the running leaf is still in the tree");
        assert_eq!(nap.status, Some(StatusKind::Running));
        let cells = layout(&top, 0, 0, 120, 24);
        let nap_cell = cells
            .iter()
            .find(|c| c.path.as_deref() == Some("prime.fan.nap"))
            .unwrap_or_else(|| panic!("the running leaf folded away: {cells:#?}"));
        assert!(!nap_cell.merged, "cells were {cells:#?}");
        assert!(nap_cell.rect.w >= 2 && nap_cell.rect.h >= 1, "{nap_cell:?}");
    }

    #[test]
    fn a_while_model_loops_own_check_tokens_get_a_leaf_alongside_its_passes() {
        use crate::model::flatten_state;
        use serde_json::json;

        // A `while: mode: model` loop writes its own `tokens_sent_
        // total`/`tokens_received_total` directly onto the loop's own
        // meta, every pass, alongside its real children (`iter_0`,
        // `iter_1`, ...) -- those tokens must not vanish into an
        // unexplained gap between this tree and the run's own totals.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "fan": {"value": null, "meta": {
                "created_at": "t0", "completed_at": "t1",
                "adapter": "scripted", "tokens_sent_total": 40, "tokens_received_total": 10
            },
                "iter_0": {"value": null, "meta": {"completed_at": "t1"},
                    "nap": prompt_node(100, 50, None, false)
                }
            }
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        let fan = &top[0];
        assert_eq!(fan.key, "fan");
        assert_eq!(fan.total, 200, "the loop's own 50 tokens plus nap's 150");
        let check = fan
            .children
            .iter()
            .find(|c| c.key == "check")
            .expect("the loop's own tokens get a leaf of their own");
        assert_eq!(check.path, "prime.fan");
        assert_eq!(check.total, 50);

        let stores = crate::cost::largest_stores("prime", &flat, &statuses, &kinds_map, None);
        let check_row = stores
            .rows
            .iter()
            .find(|r| r.path == "prime.fan" && r.tokens_sent == 40)
            .unwrap_or_else(|| {
                panic!(
                    "the table must also carry the loop's own row: {:#?}",
                    stores.rows
                )
            });
        assert_eq!(
            check_row.label, "fan › check",
            "unzoomed, the loop's own row reads as its check leaf, not the loop itself"
        );

        // Zoomed into the loop itself, the row's own path equals the
        // zoom root, so there is no prefix left to strip -- the label
        // must still read `check`, never the unstripped full path.
        assert_eq!(
            crate::cost::zoomed_row_label(check_row, "prime.fan"),
            "check"
        );
    }

    #[test]
    fn a_container_with_a_finished_child_and_a_running_child_reads_the_real_total() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null},
                "done": prompt_node(70, 30, None, false),
                "running": prompt_node(0, 0, None, true)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        statuses.insert(
            "prime.fan".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "fan");
        assert_eq!(
            top[0].total, 100,
            "the real, unboosted total -- the finished child's 100, never the running child's own boosted weight folded in"
        );
    }

    #[test]
    fn a_container_with_only_a_running_child_reads_zero_but_still_draws() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null},
                "running": prompt_node(0, 0, None, true)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        statuses.insert(
            "prime.fan".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(
            top[0].total, 0,
            "nothing real reported yet -- a front end reads this as \"tokens when done\", never a number"
        );
        assert!(
            top[0].weight > 0,
            "still drawable: a nonzero layout weight even though the real total is zero"
        );
    }

    #[test]
    fn a_container_merely_running_with_an_adapter_key_of_its_own_gets_no_check_tile() {
        use crate::model::flatten_state;
        use serde_json::json;

        // The bug this guards against: both engines record an
        // `adapter` key on nearly every container's own meta (the
        // root's included), not only a genuine `while: mode: model`
        // loop's -- `is_token_bearing_kind` alone, the rule a *leaf*
        // uses, would wrongly treat any such running container as a
        // store reporting \"tokens when done\" of its own.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "adapter": "scripted"},
                "nap": prompt_node(10, 5, None, false)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        statuses.insert(
            "prime.fan".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "fan");
        assert_eq!(top[0].total, 15, "only nap's own real tokens");
        assert!(
            top[0].children.iter().all(|c| c.key != "check"),
            "a container merely carrying an `adapter` key, never having reported its own value, gets no check tile: {:#?}",
            top[0].children
        );
    }

    #[test]
    fn the_root_never_gets_a_tile_of_its_own_even_when_its_meta_reports_a_value() {
        use crate::model::flatten_state;
        use serde_json::json;

        // The root itself writes `tokens_sent_total`/`tokens_received_
        // total` the same way a `while: mode: model` loop's own meta
        // does (an edge case: the document's own root effect list
        // behaving like one) -- it must still never surface as a
        // top-level `check` tile alongside the real stores.
        let state = json!({"prime": {"value": null, "meta": {
            "completed_at": "t1", "adapter": "scripted", "tokens_sent_total": 40, "tokens_received_total": 10
        },
            "a": prompt_node(70, 30, None, false)
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert!(
            top.iter().all(|n| n.path != "prime"),
            "the root must never get a tile of its own: {top:#?}"
        );
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "a");
    }

    #[test]
    fn chain_folding_follows_the_breadcrumb_eligibility_rule_not_status_agreement_alone() {
        use crate::model::flatten_state;
        use serde_json::json;

        // `fan` is a loop container (`RowKind::Loop`): even though its
        // one real child (`iter_0`'s own folded `nap`) shares its
        // status, a loop's own `n/total` progress must not vanish into
        // a breadcrumb -- the orchestration pane never folds a loop
        // row away either.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null},
                "iter_0": {"value": null, "meta": {"completed_at": null},
                    "nap": prompt_node(10, 5, None, true)
                }
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let running = crate::model::RowStatus {
            kind: StatusKind::Running,
            skip_reason: None,
            retrying: false,
        };
        statuses.insert("prime.fan".to_string(), running.clone());
        statuses.insert("prime.fan.iter_0".to_string(), running.clone());
        statuses.insert("prime.fan.iter_0.nap".to_string(), running);
        let mut kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        kinds_map.insert("prime.fan".to_string(), RowKind::Loop);
        kinds_map.insert("prime.fan.iter_0".to_string(), RowKind::Pass);
        kinds_map.insert("prime.fan.iter_0.nap".to_string(), RowKind::Prompt);
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        // `fan` itself (a loop) must survive as its own tile, never
        // folded away with its single pass -- only the pass and its
        // own single child (both eligible) fold together.
        assert_eq!(top[0].key, "fan");
        assert_eq!(top[0].children.len(), 1);
        assert_eq!(top[0].children[0].key, "iter_0 \u{203a} nap");
    }

    /// Issue #643's sixth follow-up: a named `if`'s own row (`gate`)
    /// carries the Flow screen's decision glyph and condition, so
    /// `kind_breadcrumb_eligible` excludes `RowKind::If` -- shared
    /// with this treemap's own chain folding, so `gate`'s single
    /// real child (`chosen`) must stay its own tile too, never
    /// folded into one `gate \u{203a} chosen` tile.
    #[test]
    fn a_named_ifs_single_child_does_not_fold_into_a_breadcrumb() {
        use crate::model::flatten_state;
        use serde_json::json;

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "gate": {"value": {"branch": "then"}, "meta": {"completed_at": "t1", "branch": "then"},
                "chosen": prompt_node(10, 5, None, false)
            }
        }});
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let done = crate::model::RowStatus {
            kind: StatusKind::Done,
            skip_reason: None,
            retrying: false,
        };
        statuses.insert("prime.gate".to_string(), done.clone());
        statuses.insert("prime.gate.chosen".to_string(), done);
        let mut kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        kinds_map.insert("prime.gate".to_string(), RowKind::If);
        kinds_map.insert("prime.gate.chosen".to_string(), RowKind::Prompt);
        let (top, _) =
            build_tree_for_measure("prime", &flat, &statuses, &kinds_map, Measure::Total);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].key, "gate");
        assert_eq!(top[0].children.len(), 1);
        assert_eq!(top[0].children[0].key, "chosen");
    }
}
