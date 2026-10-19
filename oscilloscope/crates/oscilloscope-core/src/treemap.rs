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
/// `pub` so a front end's own selection order (issue #632: "↑/↓
/// select tiles in size order") can match exactly what [`layout`]
/// itself lays tiles out in, rather than keeping its own, possibly
/// diverging, copy of this rule.
pub fn sorted_children(children: &[TreemapNode]) -> Vec<&TreemapNode> {
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
    (root.map(|r| r.children).unwrap_or_default(), excluded)
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
    // `while.template` row) -- a container that reports a value of
    // its own is a store in exactly the same sense its children are,
    // so it gets a leaf tile of its own alongside them rather than
    // vanishing into an unexplained gap between this screen's total
    // and `runtime.last_run.totals`.
    if let Some(own_leaf) = leaf_node_for_measure(path, flat, status, measure, &mut 0) {
        children.push(TreemapNode {
            key: "check".to_string(),
            ..own_leaf
        });
    }
    boost_running_placeholders(&mut children);
    let total: u64 = children.iter().map(|c| c.total).sum();
    if total == 0 {
        return None;
    }
    let key = path.rsplit('.').next().unwrap_or(path).to_string();
    Some(TreemapNode {
        key,
        path: path.to_string(),
        total,
        status,
        children,
    })
}

/// The leaf-building half of [`build_node_for_measure`], shared with
/// a container's own leaf tile (the `while: mode: model` case
/// above) -- `excluded` is passed `&mut 0` for that second use, since
/// a container already left out of the tree for scoring zero under
/// `measure` is counted once, as a container, by its own caller, not
/// a second time for an own-value leaf that was never added at all.
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
        let total = own_measure_value(node, measure).max(RUNNING_PLACEHOLDER_WEIGHT);
        return Some(TreemapNode {
            key,
            path: path.to_string(),
            total,
            status,
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
        status,
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
/// `total` is the placeholder value unchanged, bubbled straight up
/// through however many containers wrap it, however far from its own
/// real siblings that puts it. This boosts *any* child still sitting
/// at the bare placeholder weight -- leaf or container, whatever its
/// own status -- to its siblings' own median total above that floor,
/// which [`layout`] then treats the same as any other real weight: a
/// visible share of the row, never a number a front end actually
/// shows (gated on `status`/`has_children` alone, same as every
/// other running leaf). A sibling group with nothing above the floor
/// either (every child still running, or genuinely all at 1 token)
/// gets no boost at all -- there is nothing realer to measure against
/// yet.
fn boost_running_placeholders(children: &mut [TreemapNode]) {
    let real_totals: Vec<u64> = children
        .iter()
        .map(|c| c.total)
        .filter(|&t| t > RUNNING_PLACEHOLDER_WEIGHT)
        .collect();
    let Some(median) = median_total(&real_totals) else {
        return;
    };
    for child in children.iter_mut() {
        if child.total <= RUNNING_PLACEHOLDER_WEIGHT {
            child.total = median;
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
            status: child.status,
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
        assert!(top[0].total > 0, "a running store still gets a cell");
        assert_eq!(excluded, 0, "a running store is never counted as excluded");
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
        assert!(
            stores
                .rows
                .iter()
                .any(|r| r.path == "prime.fan" && r.tokens_sent == 40),
            "the table must also carry the loop's own row: {:#?}",
            stores.rows
        );
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
}
