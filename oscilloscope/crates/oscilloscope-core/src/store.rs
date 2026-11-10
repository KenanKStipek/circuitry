//! The Store screen's own area diagram, "Largest Stores" table and
//! Readings (design v1 part 4, issue #695), replacing the old Cost
//! screen's treemap/chart/table (DESIGN.md §6.3b, now superseded).
//! Pure data and pure integer geometry, the same `oscilloscope`-only
//! drawing split `treemap.rs`/`layout.rs` already followed: `tui.rs`
//! only ever paints the rectangles and text this module already
//! decided, never lays anything out itself.
//!
//! A store is a step (a leaf) that has used tokens so far. A
//! container -- a decision (`if`), a loop, a parallel `dynamic`, or
//! a `use` -- holds its children's tiles inside a darker surface; a
//! loop pass (`iter_N`) folds into its own single token-bearing step
//! when it has exactly one, so `drafts › iter_0` reads as `iter_0 ›
//! draft` rather than drawing a container one level deeper for
//! nothing. Nothing is drawn for a store, or a whole container, that
//! has not used any tokens at all.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{NodeMeta, RunModel, node_tokens, prompt_live_figures};
use crate::render::RowKind;

/// A container kind in the Store's area diagram (DESIGN.md §6.3e,
/// issue #695's settled point 1): a decision, a loop, a parallel
/// `dynamic` or a `use`. Everything else (a tool, a prompt, a loop
/// pass before it is known to fold, `try`/`finally`) is never a
/// container of its own here -- a `try`/`finally` body has no tile
/// of its own on the Store either way, its children attaching
/// straight to its own parent, the same transparent treatment the
/// Flow rail already gives it (DESIGN.md's Flow paragraph: "a
/// sequential one ... inlines its own children").
fn is_container_kind(kind: RowKind) -> bool {
    matches!(
        kind,
        RowKind::If | RowKind::Loop | RowKind::Dynamic | RowKind::Use
    )
}

/// Whether `path`'s own last segment is a loop pass placeholder
/// (`iter_N`) -- the same shape `tape_row_is_under_a_queued_pass`
/// reads, checked here independent of `kinds` so a pass still folds
/// correctly even where nothing else ever gave it a `RowKind::Pass`
/// row of its own (a tree-flow loop's un-dispatched pass, say).
fn is_pass_segment(path: &str) -> bool {
    path.rsplit('.').next().is_some_and(|seg| {
        seg.strip_prefix("iter_")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// A store that has used tokens so far -- a leaf of the area diagram
/// (DESIGN.md §2.4: `kind · 215 t`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoreLeaf {
    /// The real, full dotted state path -- never a pass's own
    /// path once folded (issue #695's settled point 1): the leaf
    /// this fold actually names, so history, selection and `enter`
    /// all resolve against a real step.
    pub path: String,
    /// The tile's own label: the leaf's bare key (`kind`, `reply`),
    /// or, once a loop pass has folded into its single token-bearing
    /// step, that step's own breadcrumb relative to the pass's
    /// parent (`iter_0 › draft`).
    pub label: String,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    /// `+6%`/`-16%`, from history's median tokens for this literal
    /// path (`screens::tape_row_usual_tokens`) -- `None` without
    /// enough history, drawn as `—` (§3.6).
    pub deviation_pct: Option<f64>,
    /// Whether this leaf's own figures are still a running estimate
    /// (issue #695's rule 13) -- drawn faint, settling once the
    /// node's own exact totals replace them.
    pub live: bool,
}

impl StoreLeaf {
    pub fn tokens(&self) -> u64 {
        self.tokens_sent + self.tokens_received
    }
}

/// A container holding its own children's tiles (DESIGN.md §2.4): a
/// decision, loop, `dynamic` or `use`, or an unfolded loop pass
/// (`iter_N`, when it has more than one token-bearing step).
#[derive(Debug, Clone, PartialEq)]
pub struct StoreContainer {
    pub path: String,
    /// `gate`, `drafts`, or an unfolded pass's own `iter_0`.
    pub label: String,
    /// Whether this container's own node is still running (issue
    /// #695's rule 6/12): draws with `●` and the accent colour in
    /// its title until the run itself ends, never afterward even if
    /// the node's own `completed_at` is still unset (an aborted
    /// container).
    pub running: bool,
    pub children: Vec<StoreNode>,
    /// How many of this container's own direct children have used
    /// tokens so far -- the `children` this struct actually carries,
    /// counted here too so the "too small for its children" summary
    /// (`children_with_tokens`/`children_planned`) still has it once
    /// the layout drops `children` from what it draws.
    pub children_with_tokens: usize,
    /// How many direct children this container's own state tree has
    /// at all, token-bearing or not (issue #695's rule 5: "a
    /// container too small for its children reads `2/3 with
    /// tokens`").
    pub children_planned: usize,
}

/// One node of the Store's own tree: a token-bearing leaf, or a
/// container holding further nodes.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreNode {
    Leaf(StoreLeaf),
    Container(StoreContainer),
}

impl StoreNode {
    pub fn path(&self) -> &str {
        match self {
            StoreNode::Leaf(l) => &l.path,
            StoreNode::Container(c) => &c.path,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            StoreNode::Leaf(l) => &l.label,
            StoreNode::Container(c) => &c.label,
        }
    }

    /// The area this node draws with -- a leaf's own tokens, or the
    /// sum of its children's (DESIGN.md §2.4: "area is its tokens so
    /// far").
    pub fn tokens(&self) -> u64 {
        match self {
            StoreNode::Leaf(l) => l.tokens(),
            StoreNode::Container(c) => c.children.iter().map(StoreNode::tokens).sum(),
        }
    }
}

/// `path`'s own direct children in the real state tree -- the same
/// walk the old Cost treemap's `direct_children_of` did, kept here
/// now that `treemap.rs` is gone (issue #695's removal, settled
/// point 7).
fn direct_children_of(path: &str, flat: &BTreeMap<String, NodeMeta>) -> Vec<String> {
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

/// How many token-bearing leaves a (already-filtered) subtree holds
/// in total -- a loop pass folds into its single step exactly when
/// this is `1`, counted across however many containers actually
/// stand between the pass and that one leaf.
fn total_leaves(node: &StoreNode) -> usize {
    match node {
        StoreNode::Leaf(_) => 1,
        StoreNode::Container(c) => c.children.iter().map(total_leaves).sum(),
    }
}

/// Drills down to the one leaf a `total_leaves(..) == 1` subtree
/// holds -- never called otherwise.
fn only_leaf(node: StoreNode) -> StoreLeaf {
    match node {
        StoreNode::Leaf(l) => l,
        StoreNode::Container(c) => only_leaf(
            c.children
                .into_iter()
                .next()
                .expect("total_leaves(..) == 1 guarantees exactly one child"),
        ),
    }
}

/// The root's own children, each either a token-bearing leaf or a
/// container holding further ones -- nothing at all for a store
/// (leaf or whole container) that has used no tokens so far
/// (DESIGN.md §2.4/§1.1: "draws only tokens used so far").
#[allow(clippy::too_many_arguments)]
pub fn build_store_tree(
    root_path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    kinds: &BTreeMap<String, RowKind>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
    model: &RunModel,
    now: &str,
    run_ended: bool,
) -> Vec<StoreNode> {
    build_children(root_path, flat, kinds, plan, header, model, now, run_ended)
}

/// `path`'s own direct children, each turned into a node (a pass, a
/// container, a leaf), or -- for a structurally nested step that is
/// none of the four container kinds (a `try`/`finally` body, chiefly)
/// -- spliced straight into this list instead of getting a tile of
/// its own, the same transparent treatment the Flow rail already
/// gives a sequential wrapper (DESIGN.md's Flow paragraph: "inlines
/// its own children as plain stations").
#[allow(clippy::too_many_arguments)]
fn build_children(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    kinds: &BTreeMap<String, RowKind>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
    model: &RunModel,
    now: &str,
    run_ended: bool,
) -> Vec<StoreNode> {
    direct_children_of(path, flat)
        .into_iter()
        .flat_map(|child| {
            build_node_or_splice(&child, flat, kinds, plan, header, model, now, run_ended)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_node_or_splice(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    kinds: &BTreeMap<String, RowKind>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
    model: &RunModel,
    now: &str,
    run_ended: bool,
) -> Vec<StoreNode> {
    if is_pass_segment(path) {
        return build_pass(path, flat, kinds, plan, header, model, now, run_ended)
            .into_iter()
            .collect();
    }
    let has_structural_children = !direct_children_of(path, flat).is_empty();
    if has_structural_children {
        if kinds.get(path).copied().is_some_and(is_container_kind) {
            return build_container(path, flat, kinds, plan, header, model, now, run_ended, None)
                .into_iter()
                .collect();
        }
        return build_children(path, flat, kinds, plan, header, model, now, run_ended);
    }
    build_leaf(path, flat, plan, header, model, now)
        .map(StoreNode::Leaf)
        .into_iter()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_container(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    kinds: &BTreeMap<String, RowKind>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
    model: &RunModel,
    now: &str,
    run_ended: bool,
    label_override: Option<String>,
) -> Option<StoreNode> {
    let children = build_children(path, flat, kinds, plan, header, model, now, run_ended);
    if children.is_empty() {
        return None;
    }
    let children_planned = direct_children_of(path, flat).len();
    let children_with_tokens = children.len();
    let running = !run_ended && flat.get(path).is_some_and(|n| n.completed_at.is_none());
    let label =
        label_override.unwrap_or_else(|| path.rsplit('.').next().unwrap_or(path).to_string());
    Some(StoreNode::Container(StoreContainer {
        path: path.to_string(),
        label,
        running,
        children,
        children_with_tokens,
        children_planned,
    }))
}

/// A loop pass path, built as a container first (so any further
/// nesting inside it still applies normally), then folded into its
/// own single token-bearing step when it has exactly one (issue
/// #695's settled point 2) -- the tool `pause`, with no tokens of
/// its own, never counts either way, since it never even builds a
/// node at all.
#[allow(clippy::too_many_arguments)]
fn build_pass(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    kinds: &BTreeMap<String, RowKind>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
    model: &RunModel,
    now: &str,
    run_ended: bool,
) -> Option<StoreNode> {
    let children = build_children(path, flat, kinds, plan, header, model, now, run_ended);
    if children.is_empty() {
        return None;
    }
    let leaves = children.iter().map(total_leaves).sum::<usize>();
    if leaves == 1 {
        let parent = path.rsplit_once('.').map(|(p, _)| p).unwrap_or("");
        let leaf = only_leaf(
            children
                .into_iter()
                .next()
                .expect("exactly one leaf means exactly one child holds it"),
        );
        let label = crate::format::path_breadcrumb_relative_to(&leaf.path, parent);
        return Some(StoreNode::Leaf(StoreLeaf { label, ..leaf }));
    }
    let children_planned = direct_children_of(path, flat).len();
    let children_with_tokens = children.len();
    let running = !run_ended && flat.get(path).is_some_and(|n| n.completed_at.is_none());
    let label = path.rsplit('.').next().unwrap_or(path).to_string();
    Some(StoreNode::Container(StoreContainer {
        path: path.to_string(),
        label,
        running,
        children,
        children_with_tokens,
        children_planned,
    }))
}

/// A leaf step's own tile data, or `None` for a step with no tokens
/// at all so far (a tool, a step still ahead, or a running step with
/// nothing to show yet -- issue #695's rule 13).
fn build_leaf(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
    model: &RunModel,
    now: &str,
) -> Option<StoreLeaf> {
    let node = flat.get(path)?;
    let label = path.rsplit('.').next().unwrap_or(path).to_string();
    let (tokens_sent, tokens_received, live) = if node.completed_at.is_some() {
        let (sent, received) = node_tokens(&node.meta);
        (sent, received, false)
    } else if let Some(agent_live) = &node.live {
        (
            agent_live.tokens_sent.unwrap_or(0),
            agent_live.tokens_received.unwrap_or(0),
            true,
        )
    } else if let Some(live) = prompt_live_figures(Some(node), path, model, now) {
        let (sent, sent_live) = live.tokens_sent_or_estimate().unwrap_or((0, false));
        let (received, received_live) = live.tokens_received_or_estimate().unwrap_or((0, false));
        (sent, received, sent_live || received_live)
    } else {
        (0, 0, false)
    };
    if tokens_sent == 0 && tokens_received == 0 {
        return None;
    }
    let deviation_pct = {
        let total = tokens_sent + tokens_received;
        crate::screens::tape_row_usual_tokens(path, plan, header)
            .filter(|&u| u > 0)
            .map(|usual| (total as f64 - usual as f64) / usual as f64 * 100.0)
    };
    Some(StoreLeaf {
        path: path.to_string(),
        label,
        tokens_sent,
        tokens_received,
        deviation_pct,
        live,
    })
}

// ---------------------------------------------------------------
// Layout: plan order, stacked children, largest-remainder areas,
// one-cell gutters, container padding (issue #695's settled point
// 1; the exact rectangles are pinned in `tests::layout` against the
// frames' own geometry).
// ---------------------------------------------------------------

/// A plain integer rectangle, cells -- deliberately not
/// `ratatui::layout::Rect` (`oscilloscope-core` draws nothing of its
/// own), same spirit as the old `treemap::Rect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// A container tile too small for even its title and padding, or
/// whose children's own apportioned sizes would fall under
/// [`MIN_TILE_WIDTH`]/[`MIN_TILE_HEIGHT`] (issue #695's rule 5):
/// drawn with only its title and `<children_with_tokens>/
/// <children_planned> with tokens`, no nested tiles at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCollapsedContainer {
    pub children_with_tokens: usize,
    pub children_planned: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreTileKind {
    Leaf(StoreLeaf),
    Container {
        running: bool,
        collapsed: Option<StoreCollapsedContainer>,
    },
    /// `… N more · T t` (issue #695's rule 5): a top-level tile too
    /// small to draw on its own, merged with every other such
    /// sibling into one trailing tile sized by their combined
    /// tokens. Never selectable in the table (it names no single
    /// store), and never itself nested.
    More {
        count: usize,
        tokens: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoreTile {
    pub path: String,
    pub label: String,
    pub rect: StoreRect,
    pub tokens: u64,
    pub kind: StoreTileKind,
}

/// A tile's own minimum columns before its container collapses
/// instead of drawing it (issue #695's rule 5: "fewer than 6
/// columns").
const MIN_TILE_WIDTH: u16 = 6;
/// A tile's own minimum rows before its container collapses instead
/// of drawing it (issue #695's rule 5: "less than one text row").
const MIN_TILE_HEIGHT: u16 = 1;

/// Largest-remainder apportionment of `total_cells` across
/// `weights`, in their own given order (never sorted by size) --
/// floors each share, then hands the leftover cells one at a time to
/// whichever share's own fractional part is largest, ties broken by
/// the earlier index. `weights` summing to zero, or `total_cells ==
/// 0`, gives every share `0`.
pub(crate) fn apportion(weights: &[u64], total_cells: u16) -> Vec<u16> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let sum: u64 = weights.iter().sum();
    if sum == 0 || total_cells == 0 {
        return vec![0; n];
    }
    let exact: Vec<f64> = weights
        .iter()
        .map(|&w| w as f64 * total_cells as f64 / sum as f64)
        .collect();
    let mut sizes: Vec<u16> = exact.iter().map(|&e| e.floor() as u16).collect();
    let allocated: u16 = sizes.iter().sum();
    let mut remainder = total_cells.saturating_sub(allocated);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let fa = exact[a] - exact[a].floor();
        let fb = exact[b] - exact[b].floor();
        fb.partial_cmp(&fa).unwrap().then(a.cmp(&b))
    });
    for &i in &order {
        if remainder == 0 {
            break;
        }
        sizes[i] += 1;
        remainder -= 1;
    }
    sizes
}

/// How many one-cell gutters `n` siblings need between them --
/// `n - 1`, or `0` for zero or one sibling.
fn gutters(n: usize) -> u16 {
    if n > 1 { (n - 1) as u16 } else { 0 }
}

/// The Store's own area diagram, laid out into `w`×`h` cells at
/// `(x, y)` (issue #695's settled point 1): `nodes` in plan order,
/// alternating columns (even depth) and rows (odd depth), areas by
/// largest-remainder apportionment of each node's own tokens, with
/// one-cell gutters between siblings and, inside a container, a
/// title row, one-cell left/right padding and a bottom padding row
/// around its own children.
pub fn layout_store(nodes: &[StoreNode], x: u16, y: u16, w: u16, h: u16) -> Vec<StoreTile> {
    layout_siblings(nodes, x, y, w, h, 0, true)
}

fn layout_siblings(
    nodes: &[StoreNode],
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    depth: u16,
    top_level: bool,
) -> Vec<StoreTile> {
    if nodes.is_empty() || w == 0 || h == 0 {
        return Vec::new();
    }
    let columns = depth % 2 == 0;
    let span = if columns { w } else { h };
    let min_needed = if columns {
        MIN_TILE_WIDTH
    } else {
        MIN_TILE_HEIGHT
    };
    let weights: Vec<u64> = nodes.iter().map(StoreNode::tokens).collect();

    if top_level {
        let content_span = span.saturating_sub(gutters(nodes.len()));
        let sizes = apportion(&weights, content_span);
        let small: Vec<usize> = sizes
            .iter()
            .enumerate()
            .filter(|&(_, &s)| s < min_needed)
            .map(|(i, _)| i)
            .collect();
        if !small.is_empty() && small.len() < nodes.len() {
            let kept: Vec<&StoreNode> = nodes
                .iter()
                .enumerate()
                .filter(|(i, _)| !small.contains(i))
                .map(|(_, n)| n)
                .collect();
            let merged_tokens: u64 = small.iter().map(|&i| weights[i]).sum();
            let merged_count = small.len();
            let mut combined_weights: Vec<u64> = kept.iter().map(|n| n.tokens()).collect();
            combined_weights.push(merged_tokens.max(1));
            let combined_span = span.saturating_sub(gutters(kept.len() + 1));
            let combined_sizes = apportion(&combined_weights, combined_span);
            let mut out = Vec::new();
            let mut cursor = if columns { x } else { y };
            for (node, &size) in kept.iter().zip(combined_sizes.iter()) {
                let rect = if columns {
                    StoreRect {
                        x: cursor,
                        y,
                        w: size,
                        h,
                    }
                } else {
                    StoreRect {
                        x,
                        y: cursor,
                        w,
                        h: size,
                    }
                };
                out.extend(layout_node(node, rect, depth));
                cursor += size + 1;
            }
            let more_size = combined_sizes[kept.len()];
            let more_rect = if columns {
                StoreRect {
                    x: cursor,
                    y,
                    w: more_size,
                    h,
                }
            } else {
                StoreRect {
                    x,
                    y: cursor,
                    w,
                    h: more_size,
                }
            };
            out.push(StoreTile {
                path: String::new(),
                label: String::new(),
                rect: more_rect,
                tokens: merged_tokens,
                kind: StoreTileKind::More {
                    count: merged_count,
                    tokens: merged_tokens,
                },
            });
            return out;
        }
    }

    let content_span = span.saturating_sub(gutters(nodes.len()));
    let sizes = apportion(&weights, content_span);
    let mut out = Vec::new();
    let mut cursor = if columns { x } else { y };
    for (node, &size) in nodes.iter().zip(sizes.iter()) {
        let rect = if columns {
            StoreRect {
                x: cursor,
                y,
                w: size,
                h,
            }
        } else {
            StoreRect {
                x,
                y: cursor,
                w,
                h: size,
            }
        };
        out.extend(layout_node(node, rect, depth));
        cursor += size + 1;
    }
    out
}

fn collapsed_tile(rect: StoreRect, c: &StoreContainer) -> StoreTile {
    StoreTile {
        path: c.path.clone(),
        label: c.label.clone(),
        rect,
        tokens: c.children.iter().map(StoreNode::tokens).sum(),
        kind: StoreTileKind::Container {
            running: c.running,
            collapsed: Some(StoreCollapsedContainer {
                children_with_tokens: c.children_with_tokens,
                children_planned: c.children_planned,
            }),
        },
    }
}

fn layout_node(node: &StoreNode, rect: StoreRect, depth: u16) -> Vec<StoreTile> {
    match node {
        StoreNode::Leaf(leaf) => vec![StoreTile {
            path: leaf.path.clone(),
            label: leaf.label.clone(),
            rect,
            tokens: leaf.tokens(),
            kind: StoreTileKind::Leaf(leaf.clone()),
        }],
        StoreNode::Container(c) => {
            if rect.w <= 2 || rect.h <= 2 {
                return vec![collapsed_tile(rect, c)];
            }
            let inner = StoreRect {
                x: rect.x + 1,
                y: rect.y + 1,
                w: rect.w - 2,
                h: rect.h - 2,
            };
            let child_depth = depth + 1;
            let columns = child_depth % 2 == 0;
            let span = if columns { inner.w } else { inner.h };
            let min_needed = if columns {
                MIN_TILE_WIDTH
            } else {
                MIN_TILE_HEIGHT
            };
            let gutter_cells = gutters(c.children.len());
            if span <= gutter_cells {
                return vec![collapsed_tile(rect, c)];
            }
            let content_span = span - gutter_cells;
            let weights: Vec<u64> = c.children.iter().map(StoreNode::tokens).collect();
            let sizes = apportion(&weights, content_span);
            if sizes.iter().any(|&s| s < min_needed) {
                return vec![collapsed_tile(rect, c)];
            }
            let mut out = vec![StoreTile {
                path: c.path.clone(),
                label: c.label.clone(),
                rect,
                tokens: c.children.iter().map(StoreNode::tokens).sum(),
                kind: StoreTileKind::Container {
                    running: c.running,
                    collapsed: None,
                },
            }];
            let mut cursor = if columns { inner.x } else { inner.y };
            for (child, &size) in c.children.iter().zip(sizes.iter()) {
                let child_rect = if columns {
                    StoreRect {
                        x: cursor,
                        y: inner.y,
                        w: size,
                        h: inner.h,
                    }
                } else {
                    StoreRect {
                        x: inner.x,
                        y: cursor,
                        w: inner.w,
                        h: size,
                    }
                };
                out.extend(layout_node(child, child_rect, child_depth));
                cursor += size + 1;
            }
            out
        }
    }
}

// ---------------------------------------------------------------
// The "Largest Stores" table and Readings (DESIGN.md §2.4).
// ---------------------------------------------------------------

/// One row of the "Largest Stores" table (DESIGN.md §2.4): the
/// store's own full path from the run's root, cut on the right with
/// `⋯` by a caller that knows how wide the table pane actually is --
/// this struct always carries the full, uncut label.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreTableRow {
    pub path: String,
    /// `drafts › iter_1 › draft` -- the full breadcrumb from the
    /// run's own root (issue #695's settled point 1: "a flat table
    /// needs it").
    pub label: String,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    /// Share of the run's tokens so far, 0-100 (the Tape's own
    /// rounding, `format::format_share_pct`).
    pub share_pct: f64,
    pub deviation_pct: Option<f64>,
}

impl StoreTableRow {
    pub fn tokens(&self) -> u64 {
        self.tokens_sent + self.tokens_received
    }
}

/// Collects every leaf of `nodes` into one flat list, with its own
/// full breadcrumb relative to `root_path` (DESIGN.md: "drafts ›
/// iter_1 › draft").
fn flatten_leaves(nodes: &[StoreNode], root_path: &str, out: &mut Vec<StoreTableRow>) {
    for node in nodes {
        match node {
            StoreNode::Leaf(leaf) => {
                out.push(StoreTableRow {
                    path: leaf.path.clone(),
                    label: crate::format::path_breadcrumb_relative_to(&leaf.path, root_path),
                    tokens_sent: leaf.tokens_sent,
                    tokens_received: leaf.tokens_received,
                    share_pct: 0.0,
                    deviation_pct: leaf.deviation_pct,
                });
            }
            StoreNode::Container(c) => flatten_leaves(&c.children, root_path, out),
        }
    }
}

/// The "Largest Stores" table (DESIGN.md §2.4, issue #695's settled
/// point 4): every store with tokens, ranked by tokens descending,
/// ties broken in plan order (the tree's own child order, which
/// `build_store_tree` already preserves) -- share is of the run's
/// own tokens so far.
pub fn build_store_table(
    nodes: &[StoreNode],
    root_path: &str,
    run_tokens: u64,
) -> Vec<StoreTableRow> {
    let mut rows = Vec::new();
    flatten_leaves(nodes, root_path, &mut rows);
    let plan_order: BTreeMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.path.clone(), i))
        .collect();
    rows.sort_by(|a, b| {
        b.tokens()
            .cmp(&a.tokens())
            .then_with(|| plan_order[&a.path].cmp(&plan_order[&b.path]))
    });
    for row in &mut rows {
        row.share_pct = if run_tokens > 0 {
            row.tokens() as f64 / run_tokens as f64 * 100.0
        } else {
            0.0
        };
    }
    rows
}

/// Readings (DESIGN.md §2.4, issue #695's settled point 4): Largest,
/// Stores with tokens, Ratio sent to received, and the running (or
/// last-run) loop's own progress.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoreReadings {
    /// The table's own first row: its label and total tokens.
    pub largest: Option<(String, u64, f64)>,
    /// How many leaves have used tokens so far.
    pub stores_with_tokens: usize,
    /// Sent to received, to one decimal (the Signal's own rounding)
    /// -- `None` with nothing received yet.
    pub ratio: Option<f64>,
    /// The running loop, else the last one that ran: its own label,
    /// how many of its passes have tokens, and the plan's own total
    /// pass count -- absent with no loop in the document at all.
    pub loop_progress: Option<(String, usize, u64)>,
}

/// How many distinct loop passes (`iter_0`, `iter_1`, ...) under
/// `container` have used tokens, however each one folds -- a folded
/// pass's own child is a leaf several segments deeper than its own
/// `iter_N`, an unfolded one's is the pass container itself, so both
/// are counted from the first path segment past `container`'s own.
fn passes_with_tokens(container: &StoreContainer) -> usize {
    let prefix_len = container.path.len() + 1;
    let mut passes = std::collections::BTreeSet::new();
    for child in &container.children {
        let path = child.path();
        if path.len() <= prefix_len {
            continue;
        }
        let rest = &path[prefix_len..];
        let first_segment = rest.split('.').next().unwrap_or(rest);
        if first_segment.starts_with("iter_") {
            passes.insert(first_segment.to_string());
        }
    }
    passes.len()
}

/// Readings' own Loop line (issue #695's settled point 4): the
/// running loop container, else the most recently run one, read
/// straight off `rows` (so a loop whose every pass folded away still
/// has a progress figure) -- `plan::resolve_each_len`/`Row::
/// loop_progress` supplies the planned pass count `m`.
pub fn build_store_readings(
    table: &[StoreTableRow],
    nodes: &[StoreNode],
    header: &crate::render::Header,
    rows: &[crate::render::Row],
) -> StoreReadings {
    let largest = table
        .first()
        .map(|r| (r.label.clone(), r.tokens(), r.share_pct));
    let stores_with_tokens = table.len();
    let ratio = (header.tokens_received > 0)
        .then(|| (header.tokens_sent as f64 / header.tokens_received as f64 * 10.0).round() / 10.0);

    let loop_rows: Vec<&crate::render::Row> =
        rows.iter().filter(|r| r.kind == RowKind::Loop).collect();
    let running_loop = loop_rows
        .iter()
        .find(|r| r.status.kind == crate::model::StatusKind::Running);
    let chosen = running_loop.or_else(|| {
        loop_rows
            .iter()
            .rfind(|r| r.status.kind == crate::model::StatusKind::Done)
    });
    let loop_progress = chosen.and_then(|row| {
        let total = row.loop_progress.as_ref().and_then(|p| p.total)?;
        let container = find_container(nodes, &row.path)?;
        let with_tokens = passes_with_tokens(container);
        Some((row.label.clone(), with_tokens, total))
    });

    StoreReadings {
        largest,
        stores_with_tokens,
        ratio,
        loop_progress,
    }
}

fn find_container<'a>(nodes: &'a [StoreNode], path: &str) -> Option<&'a StoreContainer> {
    for node in nodes {
        match node {
            StoreNode::Container(c) if c.path == path => return Some(c),
            StoreNode::Container(c) => {
                if let Some(found) = find_container(&c.children, path) {
                    return Some(found);
                }
            }
            StoreNode::Leaf(_) => {}
        }
    }
    None
}

/// Screen 3's own model (issue #695, design v1 part 4, replacing
/// Cost): the area diagram's tree, the "Largest Stores" table and
/// Readings, built from exactly the same inputs the Tape's/Signal's/
/// Flow's own models already have -- no engine change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoreScreen {
    pub tree: Vec<StoreNode>,
    pub table: Vec<StoreTableRow>,
    pub readings: StoreReadings,
    /// The Help overlay's own token legend reads this the same way
    /// the Tape's/Signal's do -- the table's own bar unit (`▰ = 50
    /// t`).
    pub unit: u64,
    /// §3.2's status line and the Starting/Ended/Failed head rule,
    /// shared chrome with the Tape/Signal/Flow.
    pub last_finished: Option<crate::screens::SignalLastFinished>,
    pub head: crate::screens::TapeHead,
    /// The table's own cursor (`App::store_selected`, kept there
    /// since it is UI state, not this screen's own model), resolved
    /// to the same [`crate::screens::FocusedStep`] the Tape's,
    /// Signal's and Flow's own full-screen step views already build
    /// theirs from -- `None` with no row selected, or a selected path
    /// no row in `rows` resolves to.
    pub selected: Option<crate::screens::FocusedStep>,
    /// The Totals row's own deviation (§3.3), the same sum over every
    /// already-run-or-running row's own usual tokens the Tape's and
    /// Signal's own `usual_so_far_tokens` is.
    pub usual_so_far_tokens: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_store(
    rows: &[crate::render::Row],
    header: &crate::render::Header,
    flat: &BTreeMap<String, NodeMeta>,
    kinds: &BTreeMap<String, RowKind>,
    plan: &crate::plan::PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    root_path: &str,
    now: &str,
    selected_path: Option<&str>,
) -> StoreScreen {
    let run_ended = header.status != crate::model::RunStatus::Running;
    let tree = build_store_tree(root_path, flat, kinds, plan, header, model, now, run_ended);
    let run_tokens = state
        .and_then(crate::model::run_totals)
        .map(|totals| {
            totals
                .get("tokens_sent")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + totals
                    .get("tokens_received")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
        })
        .unwrap_or(header.tokens_sent + header.tokens_received);
    let table = build_store_table(&tree, root_path, run_tokens);
    let readings = build_store_readings(&table, &tree, header, rows);
    let largest_usual_step = header
        .history_step_usual
        .values()
        .filter_map(|s| s.tokens_p50)
        .max();
    let unit = crate::screens::tape_unit(largest_usual_step);
    let run_start = header.run_start_ts.as_deref();
    let selected = selected_path
        .and_then(|path| rows.iter().find(|r| r.path == path))
        .map(|row| {
            crate::screens::build_focused_step(
                row,
                flat,
                plan,
                model,
                state,
                now,
                header.run_end_ts.as_deref(),
            )
        });
    let usual_so_far_tokens = {
        use crate::model::StatusKind;
        let sum: u64 = rows
            .iter()
            .filter(|r| !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped))
            .filter_map(|r| crate::screens::tape_row_usual_tokens(&r.path, plan, header))
            .sum();
        (sum > 0).then_some(sum)
    };
    StoreScreen {
        tree,
        table,
        readings,
        unit,
        last_finished: crate::screens::last_finished_row(rows, flat, plan, run_start),
        head: crate::screens::build_run_head(run_start, header, rows, flat),
        selected,
        usual_so_far_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StatusKind;

    fn leaf(path: &str, label: &str, tokens: u64) -> StoreNode {
        StoreNode::Leaf(StoreLeaf {
            path: path.to_string(),
            label: label.to_string(),
            tokens_sent: tokens,
            tokens_received: 0,
            deviation_pct: None,
            live: false,
        })
    }

    fn container(path: &str, label: &str, running: bool, children: Vec<StoreNode>) -> StoreNode {
        let children_with_tokens = children.len();
        StoreNode::Container(StoreContainer {
            path: path.to_string(),
            label: label.to_string(),
            running,
            children,
            children_with_tokens,
            children_planned: children_with_tokens,
        })
    }

    /// The frames' own geometry (issue #695's R1): at 120x30 `kind`
    /// x2-29, `gate` x31-81 (`reply` x32-80), `drafts` x83-116
    /// (children x84-115), tiles y2-16 (`iter_0` y3-8, gutter y9,
    /// `iter_1` y10-15).
    #[test]
    fn layout_matches_the_120x30_frame_geometry() {
        let tree = vec![
            leaf("prime.kind", "kind", 215),
            container(
                "prime.gate",
                "gate",
                false,
                vec![leaf("prime.gate.reply", "reply", 389)],
            ),
            container(
                "prime.drafts",
                "drafts",
                true,
                vec![
                    leaf("prime.drafts.iter_0.draft", "iter_0 \u{203a} draft", 130),
                    leaf("prime.drafts.iter_1.draft", "iter_1 \u{203a} draft", 131),
                ],
            ),
        ];
        let tiles = layout_store(&tree, 2, 2, 115, 15);
        let kind = tiles.iter().find(|t| t.path == "prime.kind").unwrap();
        assert_eq!(
            kind.rect,
            StoreRect {
                x: 2,
                y: 2,
                w: 28,
                h: 15
            }
        );
        let gate = tiles.iter().find(|t| t.path == "prime.gate").unwrap();
        assert_eq!(
            gate.rect,
            StoreRect {
                x: 31,
                y: 2,
                w: 51,
                h: 15
            }
        );
        let reply = tiles.iter().find(|t| t.path == "prime.gate.reply").unwrap();
        assert_eq!(
            reply.rect,
            StoreRect {
                x: 32,
                y: 3,
                w: 49,
                h: 13
            }
        );
        let drafts = tiles.iter().find(|t| t.path == "prime.drafts").unwrap();
        assert_eq!(
            drafts.rect,
            StoreRect {
                x: 83,
                y: 2,
                w: 34,
                h: 15
            }
        );
        let iter0 = tiles
            .iter()
            .find(|t| t.path == "prime.drafts.iter_0.draft")
            .unwrap();
        assert_eq!(
            iter0.rect,
            StoreRect {
                x: 84,
                y: 3,
                w: 32,
                h: 6
            }
        );
        let iter1 = tiles
            .iter()
            .find(|t| t.path == "prime.drafts.iter_1.draft")
            .unwrap();
        assert_eq!(
            iter1.rect,
            StoreRect {
                x: 84,
                y: 10,
                w: 32,
                h: 6
            }
        );
    }

    /// At 80x24: `kind` x2-19, `gate` x21-53, `drafts` x55-76
    /// (children x56-75), tiles y2-11 (`iter_0` y3-5, `iter_1`
    /// y7-10) -- the extra row goes to `iter_1` (131 t > 130 t).
    #[test]
    fn layout_matches_the_80x24_frame_geometry() {
        let tree = vec![
            leaf("prime.kind", "kind", 215),
            container(
                "prime.gate",
                "gate",
                false,
                vec![leaf("prime.gate.reply", "reply", 389)],
            ),
            container(
                "prime.drafts",
                "drafts",
                true,
                vec![
                    leaf("prime.drafts.iter_0.draft", "iter_0 \u{203a} draft", 130),
                    leaf("prime.drafts.iter_1.draft", "iter_1 \u{203a} draft", 131),
                ],
            ),
        ];
        let tiles = layout_store(&tree, 2, 2, 75, 10);
        let kind = tiles.iter().find(|t| t.path == "prime.kind").unwrap();
        assert_eq!(
            kind.rect,
            StoreRect {
                x: 2,
                y: 2,
                w: 18,
                h: 10
            }
        );
        let gate = tiles.iter().find(|t| t.path == "prime.gate").unwrap();
        assert_eq!(
            gate.rect,
            StoreRect {
                x: 21,
                y: 2,
                w: 33,
                h: 10
            }
        );
        let drafts = tiles.iter().find(|t| t.path == "prime.drafts").unwrap();
        assert_eq!(
            drafts.rect,
            StoreRect {
                x: 55,
                y: 2,
                w: 22,
                h: 10
            }
        );
        let iter0 = tiles
            .iter()
            .find(|t| t.path == "prime.drafts.iter_0.draft")
            .unwrap();
        assert_eq!(
            iter0.rect,
            StoreRect {
                x: 56,
                y: 3,
                w: 20,
                h: 3
            }
        );
        let iter1 = tiles
            .iter()
            .find(|t| t.path == "prime.drafts.iter_1.draft")
            .unwrap();
        assert_eq!(
            iter1.rect,
            StoreRect {
                x: 56,
                y: 7,
                w: 20,
                h: 4
            }
        );
    }

    #[test]
    fn apportion_uses_largest_remainder_with_earliest_index_tie_break() {
        assert_eq!(apportion(&[215, 389, 261], 113), vec![28, 51, 34]);
        assert_eq!(apportion(&[215, 389, 261], 73), vec![18, 33, 22]);
        assert_eq!(apportion(&[130, 131], 12), vec![6, 6]);
        assert_eq!(apportion(&[130, 131], 7), vec![3, 4]);
    }

    #[test]
    fn apportion_gives_every_share_zero_without_weight_or_cells() {
        assert_eq!(apportion(&[0, 0], 10), vec![0, 0]);
        assert_eq!(apportion(&[5, 5], 0), vec![0, 0]);
        assert_eq!(apportion(&[], 10), Vec::<u16>::new());
    }

    #[test]
    fn a_container_too_small_for_its_children_collapses_to_a_summary() {
        let tree = vec![container(
            "prime.gate",
            "gate",
            false,
            vec![
                leaf("prime.gate.a", "a", 100),
                leaf("prime.gate.b", "b", 100),
            ],
        )];
        // Two rows stacked need at least 2 rows plus a gutter plus
        // the title/padding rows -- 4 total rows leaves only 1 row
        // for the two children after the title and bottom pad,
        // under `MIN_TILE_HEIGHT` once a gutter is subtracted.
        let tiles = layout_store(&tree, 0, 0, 20, 4);
        assert_eq!(tiles.len(), 1);
        match &tiles[0].kind {
            StoreTileKind::Container { collapsed, .. } => {
                assert_eq!(
                    *collapsed,
                    Some(StoreCollapsedContainer {
                        children_with_tokens: 2,
                        children_planned: 2,
                    })
                );
            }
            other => panic!("expected a collapsed container, got {other:?}"),
        }
    }

    #[test]
    fn too_small_top_level_stores_fold_into_one_more_tile() {
        let tree = vec![
            leaf("prime.big", "big", 1000),
            leaf("prime.tiny1", "tiny1", 1),
            leaf("prime.tiny2", "tiny2", 1),
        ];
        // 20 columns total, 2 gutters reserved: `tiny1`/`tiny2` would
        // each get far under `MIN_TILE_WIDTH` and merge.
        let tiles = layout_store(&tree, 0, 0, 20, 5);
        let more = tiles
            .iter()
            .find(|t| matches!(t.kind, StoreTileKind::More { .. }))
            .expect("the two tiny stores merge into one More tile");
        match &more.kind {
            StoreTileKind::More { count, tokens } => {
                assert_eq!(*count, 2);
                assert_eq!(*tokens, 2);
            }
            _ => unreachable!(),
        }
        assert!(tiles.iter().any(|t| t.path == "prime.big"));
    }

    #[test]
    fn zero_token_stores_and_containers_are_excluded() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "empty_tool": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1"}},
                "used": {"value": "ok", "meta": {
                    "created_at": "t0", "completed_at": "t1",
                    "tokens_sent_total": 10, "tokens_received_total": 5
                }}
            }
        }));
        let kinds = BTreeMap::new();
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree("prime", &flat, &kinds, &plan, &header, &model, "t1", true);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].path(), "prime.used");
    }

    #[test]
    fn a_pass_with_exactly_one_token_bearing_step_folds_into_a_leaf() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"},
                    "iter_0": {
                        "pause": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"}},
                        "draft": {"value": "y", "meta": {
                            "created_at": "t0", "completed_at": "t1",
                            "tokens_sent_total": 100, "tokens_received_total": 30
                        }}
                    }
                }
            }
        }));
        let mut kinds = BTreeMap::new();
        kinds.insert("prime.drafts".to_string(), RowKind::Loop);
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree("prime", &flat, &kinds, &plan, &header, &model, "t1", true);
        assert_eq!(tree.len(), 1);
        let StoreNode::Container(drafts) = &tree[0] else {
            panic!("expected drafts to stay a container");
        };
        assert_eq!(drafts.children.len(), 1);
        let StoreNode::Leaf(draft) = &drafts.children[0] else {
            panic!("the pass with exactly one token-bearing step must fold into a leaf");
        };
        assert_eq!(draft.path, "prime.drafts.iter_0.draft");
        assert_eq!(draft.label, "iter_0 \u{203a} draft");
        assert_eq!(draft.tokens(), 130);
    }

    #[test]
    fn a_pass_with_more_than_one_token_bearing_step_stays_a_container() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"},
                    "iter_0": {
                        "draft": {"value": "y", "meta": {
                            "created_at": "t0", "completed_at": "t1",
                            "tokens_sent_total": 100, "tokens_received_total": 30
                        }},
                        "critique": {"value": "y", "meta": {
                            "created_at": "t0", "completed_at": "t1",
                            "tokens_sent_total": 10, "tokens_received_total": 3
                        }}
                    }
                }
            }
        }));
        let mut kinds = BTreeMap::new();
        kinds.insert("prime.drafts".to_string(), RowKind::Loop);
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree("prime", &flat, &kinds, &plan, &header, &model, "t1", true);
        let StoreNode::Container(drafts) = &tree[0] else {
            panic!("expected drafts to stay a container");
        };
        assert_eq!(drafts.children.len(), 1);
        let StoreNode::Container(pass) = &drafts.children[0] else {
            panic!("a pass with two token-bearing steps must stay a container");
        };
        assert_eq!(pass.label, "iter_0");
        assert_eq!(pass.children.len(), 2);
    }

    #[test]
    fn a_running_prompt_gets_a_faint_live_tile_from_its_estimate() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z",
                    "adapter": "scripted",
                    "live": {
                        "adapter": "scripted", "streaming": true, "attempt": 0,
                        "tokens_sent_estimate": 40
                    }
                }}
            }
        }));
        let kinds = BTreeMap::new();
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree(
            "prime",
            &flat,
            &kinds,
            &plan,
            &header,
            &model,
            "2026-01-01T00:00:05Z",
            false,
        );
        assert_eq!(tree.len(), 1);
        let StoreNode::Leaf(ask) = &tree[0] else {
            panic!("expected a live leaf tile for the running prompt");
        };
        assert!(ask.live);
        assert_eq!(ask.tokens_sent, 40);
    }

    #[test]
    fn a_running_step_with_no_live_figures_gets_no_tile() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "adapter": "scripted"
                }}
            }
        }));
        let kinds = BTreeMap::new();
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree(
            "prime",
            &flat,
            &kinds,
            &plan,
            &header,
            &model,
            "2026-01-01T00:00:05Z",
            false,
        );
        assert!(tree.is_empty());
    }

    #[test]
    fn a_running_container_carries_the_marker_until_the_run_ends() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                    "iter_0": {
                        "draft": {"value": "y", "meta": {
                            "created_at": "t0", "completed_at": "t1",
                            "tokens_sent_total": 100, "tokens_received_total": 30
                        }}
                    }
                }
            }
        }));
        let mut kinds = BTreeMap::new();
        kinds.insert("prime.drafts".to_string(), RowKind::Loop);
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();

        let running_tree =
            build_store_tree("prime", &flat, &kinds, &plan, &header, &model, "t1", false);
        let StoreNode::Container(drafts) = &running_tree[0] else {
            panic!("expected a container");
        };
        assert!(drafts.running, "a running container carries the marker");

        let ended_tree =
            build_store_tree("prime", &flat, &kinds, &plan, &header, &model, "t1", true);
        let StoreNode::Container(drafts) = &ended_tree[0] else {
            panic!("expected a container");
        };
        assert!(
            !drafts.running,
            "the marker leaves once the run itself has ended, even with no completed_at"
        );
    }

    fn empty_header() -> crate::render::Header {
        let plan = crate::plan::PlanTree::empty();
        let mut model = RunModel::new();
        crate::render::build_folded(
            "demo.yml",
            "cof",
            &plan,
            &mut model,
            None,
            crate::model::ProcessState::Running,
            0.0,
            "2026-01-01T00:00:00Z",
            &[],
            true,
            &Default::default(),
        )
        .header
    }

    #[test]
    fn table_orders_by_tokens_desc_ties_in_plan_order_and_rounds_share() {
        let tree = vec![
            leaf("prime.a", "a", 100),
            leaf("prime.b", "b", 300),
            leaf("prime.c", "c", 100),
        ];
        let table = build_store_table(&tree, "prime", 500);
        assert_eq!(
            table.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["prime.b", "prime.a", "prime.c"]
        );
        assert_eq!(table[0].share_pct.round() as i64, 60);
    }

    #[test]
    fn table_labels_cut_the_breadcrumb_relative_to_the_root() {
        let tree = vec![container(
            "prime.drafts",
            "drafts",
            false,
            vec![leaf(
                "prime.drafts.iter_1.draft",
                "iter_1 \u{203a} draft",
                50,
            )],
        )];
        let table = build_store_table(&tree, "prime", 50);
        assert_eq!(table[0].label, "drafts \u{203a} iter_1 \u{203a} draft");
    }

    #[test]
    fn readings_report_the_running_loops_progress() {
        let tree = vec![container(
            "prime.drafts",
            "drafts",
            true,
            vec![
                leaf("prime.drafts.iter_0.draft", "iter_0 \u{203a} draft", 130),
                leaf("prime.drafts.iter_1.draft", "iter_1 \u{203a} draft", 131),
            ],
        )];
        let rows = vec![crate::render::Row {
            path: "prime.drafts".to_string(),
            depth: 1,
            label: "drafts".to_string(),
            kind: RowKind::Loop,
            status: crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: Some(crate::render::LoopProgress {
                done: 2,
                total: Some(3),
                eta_s: None,
            }),
            dim: None,
            has_children: true,
        }];
        let header = empty_header();
        let table = build_store_table(&tree, "prime", 261);
        let readings = build_store_readings(&table, &tree, &header, &rows);
        assert_eq!(readings.loop_progress, Some(("drafts".to_string(), 2, 3)));
    }

    #[test]
    fn readings_absent_without_any_loop() {
        let tree = vec![leaf("prime.kind", "kind", 10)];
        let header = empty_header();
        let table = build_store_table(&tree, "prime", 10);
        let readings = build_store_readings(&table, &tree, &header, &[]);
        assert_eq!(readings.loop_progress, None);
    }
}
