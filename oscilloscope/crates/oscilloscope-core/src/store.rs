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

use crate::model::{NodeMeta, RunModel, node_tokens};
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
    /// Whether `tokens_sent`/`tokens_received` (respectively) is a
    /// real `0`, or stands in for a figure neither live source has
    /// ever reported (Claude Code's own `tokens_received: null`
    /// mid-turn) -- drawn as `—` rather than a false `0` wherever a
    /// leaf's own sent/received split is shown; the sums and shares
    /// above still treat it as `0`, the same rule every other screen
    /// follows.
    pub tokens_sent_unknown: bool,
    pub tokens_received_unknown: bool,
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
    state: Option<&Value>,
) -> Vec<StoreNode> {
    build_children(
        root_path, flat, kinds, plan, header, model, now, run_ended, state,
    )
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
    state: Option<&Value>,
) -> Vec<StoreNode> {
    let mut children = direct_children_of(path, flat);
    sort_in_plan_order(&mut children, plan);
    children
        .into_iter()
        .flat_map(|child| {
            build_node_or_splice(
                &child, flat, kinds, plan, header, model, now, run_ended, state,
            )
        })
        .collect()
}

/// `direct_children_of`'s own walk (`subtree_entries`, a
/// `BTreeMap`) reads children alphabetically by path, never in plan
/// order -- issue #695's settled point 1 asks for plan order
/// specifically (`kind, gate, drafts`, not `drafts, gate, kind`).
/// `plan::normalize_pass_segments` turns a real pass's own `iter_N`
/// into the `iter_*` placeholder `PlanTree::all_paths` itself uses,
/// so a loop's own real passes sort together at the position their
/// shared template holds; a path the static plan never named at all
/// (a dynamically discovered `use` child, say) keeps its own
/// original relative order, stably, at the end.
fn sort_in_plan_order(children: &mut [String], plan: &crate::plan::PlanTree) {
    let order: Vec<Option<usize>> = children
        .iter()
        .map(|path| {
            let normalized = crate::plan::normalize_pass_segments(path);
            plan.all_paths().iter().position(|p| *p == normalized)
        })
        .collect();
    let mut indexed: Vec<(usize, &str)> = children.iter().map(|s| s.as_str()).enumerate().collect();
    indexed.sort_by_key(|(i, _)| order[*i].unwrap_or(usize::MAX));
    let sorted: Vec<String> = indexed.into_iter().map(|(_, s)| s.to_string()).collect();
    children.clone_from_slice(&sorted);
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
    state: Option<&Value>,
) -> Vec<StoreNode> {
    if is_pass_segment(path) {
        return build_pass(
            path, flat, kinds, plan, header, model, now, run_ended, state,
        )
        .into_iter()
        .collect();
    }
    let has_structural_children = !direct_children_of(path, flat).is_empty();
    if has_structural_children {
        if kinds.get(path).copied().is_some_and(is_container_kind) {
            return build_container(
                path, flat, kinds, plan, header, model, now, run_ended, state, None,
            )
            .into_iter()
            .collect();
        }
        return build_children(
            path, flat, kinds, plan, header, model, now, run_ended, state,
        );
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
    state: Option<&Value>,
    label_override: Option<String>,
) -> Option<StoreNode> {
    let mut children = build_children(
        path, flat, kinds, plan, header, model, now, run_ended, state,
    );
    // The plan's own `resolve_each_len` is a loop's planned pass
    // count `m` (an `each` loop's own static array length); a loop
    // with only 2 of 3 passes started so far must still read `2/3`,
    // not `2/2`, so counting the children actually present in state
    // (`direct_children_of`'s own count) only serves as a fallback
    // for a `while` loop (no static count at all), a container whose
    // own fan-out the plan can't know ahead of time, or any other
    // container kind (never has a pass count to begin with).
    let children_planned = state
        .and_then(|root| plan.resolve_each_len(path, root))
        .map(|t| t as usize)
        .unwrap_or_else(|| direct_children_of(path, flat).len());
    let children_with_tokens = direct_children_with_tokens(path, flat);
    if let Some(own) = container_own_leaf(path, flat, plan, header) {
        children.push(StoreNode::Leaf(own));
    }
    if children.is_empty() {
        return None;
    }
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

/// A decision's or loop's own tokens (issue #695: a model-mode `if`
/// or `while` spends tokens evaluating its own condition, recorded
/// on the container's own node rather than on any child -- `core/
/// conditional.py`'s and `core/loop.py`'s `mode: model` branches),
/// drawn as a `check` leaf alongside the container's real children,
/// the same leaf the old Cost treemap's `container_own_value_node`
/// drew for exactly this case.
fn container_own_leaf(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &crate::plan::PlanTree,
    header: &crate::render::Header,
) -> Option<StoreLeaf> {
    let node = flat.get(path)?;
    let (tokens_sent, tokens_received) = node_tokens(&node.meta);
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
        label: "check".to_string(),
        tokens_sent,
        tokens_received,
        tokens_sent_unknown: false,
        tokens_received_unknown: false,
        deviation_pct,
        live: false,
    })
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
    state: Option<&Value>,
) -> Option<StoreNode> {
    let children = build_children(
        path, flat, kinds, plan, header, model, now, run_ended, state,
    );
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
    let children_with_tokens = direct_children_with_tokens(path, flat);
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

/// How many of `path`'s own direct structural children have any
/// tokens anywhere in their own subtree -- counted straight from
/// state rather than from a container's own already-built
/// `children`, which can hold more nodes than there are direct
/// children at all once a wrapper with no tile of its own (a
/// `try`/`finally` body) splices its grandchildren straight in.
fn direct_children_with_tokens(path: &str, flat: &BTreeMap<String, NodeMeta>) -> usize {
    direct_children_of(path, flat)
        .into_iter()
        .filter(|child| {
            let (sent, received) = crate::model::subtree_tokens(child, flat);
            sent > 0 || received > 0
        })
        .count()
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
    // Issue #702: the same step-tokens-so-far rule every other
    // screen's sums and shares now read -- in particular, a stale
    // leftover `meta.live` (an interrupted agent session,
    // `AgentLive::is_recent` false) still gives its last known
    // figures here, just no longer drawn faint.
    let figures = crate::model::step_tokens_so_far(Some(node), path, model, now);
    let tokens_sent_unknown = figures.tokens_sent.is_none();
    let tokens_received_unknown = figures.tokens_received.is_none();
    let tokens_sent = figures.tokens_sent.unwrap_or(0);
    let tokens_received = figures.tokens_received.unwrap_or(0);
    let live = figures.live;
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
        tokens_sent_unknown,
        tokens_received_unknown,
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
/// The top-level More tile's own minimum columns -- wider than
/// MIN_TILE_WIDTH, since its own text needs room for the word
/// "more" to survive the right-hand cut, not just a count and an
/// ellipsis.
const MORE_TILE_MIN_WIDTH: u16 = 12;
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

/// Whether `kept`'s own weights would each reach [`MIN_TILE_WIDTH`]
/// once `span` is apportioned across them, reserving `MIN_TILE_WIDTH`
/// more of `span` for a trailing More tile when `has_more` is set --
/// so a merged tile's own width is checked before it is ever built,
/// rather than apportioned after the fact alongside the kept tiles'
/// own weights, where a much larger kept tile's fractional remainder
/// can starve it down to nothing.
fn top_level_tiles_fit(kept: &[usize], has_more: bool, weights: &[u64], span: u16) -> bool {
    if kept.is_empty() {
        return !has_more;
    }
    let reserved = if has_more { MORE_TILE_MIN_WIDTH } else { 0 };
    let gutter_count = gutters(kept.len() + usize::from(has_more));
    let Some(available) = span
        .checked_sub(gutter_count)
        .and_then(|s| s.checked_sub(reserved))
    else {
        return false;
    };
    let kept_weights: Vec<u64> = kept.iter().map(|&i| weights[i]).collect();
    apportion(&kept_weights, available)
        .iter()
        .all(|&s| s >= MIN_TILE_WIDTH)
}

/// Chooses which of the top level's own nodes keep a tile of their
/// own and which fold into one trailing "… N more" tile (issue
/// #695's rule 5): every node first, if the whole span already gives
/// each one at least [`MIN_TILE_WIDTH`]; otherwise the largest
/// (tokens descending, ties in plan order -- the index these
/// `weights` already carry) are kept one at a time, for as long as
/// the remaining span can still give every kept tile its own minimum
/// width *and* leave at least that same minimum for everything else,
/// merged into one trailing tile. At least one node is always kept,
/// even if its own tile would still fall under the minimum, so the
/// diagram is never left completely empty.
fn top_level_fold(weights: &[u64], span: u16) -> (Vec<usize>, Vec<usize>) {
    let n = weights.len();
    let all: Vec<usize> = (0..n).collect();
    if top_level_tiles_fit(&all, false, weights, span) {
        return (all, Vec::new());
    }
    let mut ranked: Vec<usize> = (0..n).collect();
    ranked.sort_by(|&a, &b| weights[b].cmp(&weights[a]).then(a.cmp(&b)));
    let mut kept_ranked: Vec<usize> = Vec::new();
    for &i in &ranked {
        let mut candidate = kept_ranked.clone();
        candidate.push(i);
        let has_more = candidate.len() < n;
        if top_level_tiles_fit(&candidate, has_more, weights, span) {
            kept_ranked = candidate;
        } else {
            break;
        }
    }
    if kept_ranked.is_empty() {
        kept_ranked.push(ranked[0]);
    }
    let mut kept = kept_ranked;
    kept.sort_unstable();
    let merged: Vec<usize> = (0..n).filter(|i| !kept.contains(i)).collect();
    (kept, merged)
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
    let weights: Vec<u64> = nodes.iter().map(StoreNode::tokens).collect();

    if top_level && nodes.len() > 1 {
        let (kept, merged) = top_level_fold(&weights, span);
        if !merged.is_empty() {
            let kept_nodes: Vec<&StoreNode> = kept.iter().map(|&i| &nodes[i]).collect();
            let merged_tokens: u64 = merged.iter().map(|&i| weights[i]).sum();
            let merged_count = merged.len();
            let gutter_count = gutters(kept_nodes.len() + 1);
            let reserved = MORE_TILE_MIN_WIDTH.min(span.saturating_sub(gutter_count));
            let kept_span = span.saturating_sub(gutter_count).saturating_sub(reserved);
            let kept_weights: Vec<u64> = kept_nodes.iter().map(|n| n.tokens()).collect();
            let kept_sizes = apportion(&kept_weights, kept_span);
            let mut out = Vec::new();
            let mut cursor = if columns { x } else { y };
            for (node, &size) in kept_nodes.iter().zip(kept_sizes.iter()) {
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
            let more_rect = if columns {
                StoreRect {
                    x: cursor,
                    y,
                    w: reserved,
                    h,
                }
            } else {
                StoreRect {
                    x,
                    y: cursor,
                    w,
                    h: reserved,
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
            // Issue #702's rule 4: never above 100%, even if a
            // caller's own denominator and this leaf's own figure
            // were read a redraw apart and briefly disagree.
            (row.tokens() as f64 / run_tokens as f64 * 100.0).min(100.0)
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

/// How many distinct loop passes (`iter_0`, `iter_1`, ...) directly
/// under `path` have used tokens anywhere in their own subtree, read
/// straight from state rather than from the already-filtered Store
/// tree -- a running loop whose passes have no tokens yet still has
/// a container there in state even though [`build_store_tree`] draws
/// no tile for it at all (nothing to show), so counting from the
/// tree itself would always read `0 of m` as "no loop running"
/// rather than "none of its passes have tokens yet".
fn passes_with_tokens_in_state(path: &str, flat: &BTreeMap<String, NodeMeta>) -> usize {
    direct_children_of(path, flat)
        .into_iter()
        .filter(|child| is_pass_segment(child))
        .filter(|child| {
            let (sent, received) = crate::model::subtree_tokens(child, flat);
            sent > 0 || received > 0
        })
        .count()
}

/// Readings' own Loop line (issue #695's settled point 4): the
/// running loop, else the most recently run one (any status but
/// `Pending`, so a loop that ended `FailedHandled` still gets a
/// figure) -- `plan::resolve_each_len`/`Row::loop_progress` supplies
/// the planned pass count `m`.
pub fn build_store_readings(
    table: &[StoreTableRow],
    flat: &BTreeMap<String, NodeMeta>,
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
            .rfind(|r| r.status.kind != crate::model::StatusKind::Pending)
    });
    let loop_progress = chosen.and_then(|row| {
        let total = row.loop_progress.as_ref().and_then(|p| p.total)?;
        let with_tokens = passes_with_tokens_in_state(&row.path, flat);
        Some((row.label.clone(), with_tokens, total))
    });

    StoreReadings {
        largest,
        stores_with_tokens,
        ratio,
        loop_progress,
    }
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
    /// The table's own cursor (issue #704 rule 2): the one shared
    /// `App::selected_path` itself when it names a store with tokens
    /// of its own, else the nearest ancestor that is one, else (with
    /// nothing in the whole run holding tokens yet) the table's own
    /// first row -- `None` only with an empty table.
    pub cursor_path: Option<String>,
    /// `cursor_path` resolved to the same [`crate::screens::
    /// FocusedStep`] the Tape's, Signal's and Flow's own full-screen
    /// step views already build theirs from -- `None` only alongside
    /// `cursor_path` itself.
    pub selected: Option<crate::screens::FocusedStep>,
    /// Issue #704 rule 4: the shared selection's own exact path,
    /// resolved the same way the Tape's and Signal's own full-screen
    /// step is -- never climbed to an ancestor the way `selected`
    /// (the table's own cursor, for the row/tile to highlight) is.
    /// `[`/`]` always lands on a real row with a step of its own
    /// (`App::fullscreen_step`'s own candidates), so the full-screen
    /// view reads this, not `selected`, or stepping through a store
    /// with no tokens of its own would show its nearest ancestor's
    /// content instead of the very effect just stepped to.
    pub raw_selected: Option<crate::screens::FocusedStep>,
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
    let tree = build_store_tree(
        root_path, flat, kinds, plan, header, model, now, run_ended, state,
    );
    // Issue #702's rule 4 ("a share never exceeds 100%"): a running
    // step's own live estimate (an agent's own `meta.live` tokens
    // included) must count in this denominator too, or a leaf tile's
    // own live-figure numerator can outrun it -- the exact `19000%`
    // this issue's own bug report found on the Largest Stores table.
    // `header.tokens_sent`/`_received` already prefer `run_totals`
    // over summing `flat` and fold in a cancelled run's own leftover
    // `meta.live` (an agent session that never finished an attempt,
    // so `run_totals` itself reads `0`/`0`) -- re-deriving the same
    // figure from `state` here instead would drop that leftover and
    // disagree with the Tape's own share for the same step.
    let run_tokens = header.tokens_sent
        + header.tokens_received
        + header.tokens_sent_live_estimate
        + header.tokens_received_live_estimate;
    let table = build_store_table(&tree, root_path, run_tokens);
    let readings = build_store_readings(&table, flat, header, rows);
    let largest_usual_step = header
        .history_step_usual
        .values()
        .filter_map(|s| s.tokens_p50)
        .max();
    let unit = crate::screens::tape_unit(largest_usual_step);
    let run_start = header.run_start_ts.as_deref();
    let plan_order: Vec<String> = rows.iter().map(|r| r.path.clone()).collect();
    let cursor_path = store_cursor_path(selected_path, &table, &plan_order)
        .or_else(|| {
            plan_order
                .iter()
                .find(|p| table.iter().any(|r| &r.path == *p))
                .cloned()
        })
        .or_else(|| table.first().map(|r| r.path.clone()));
    let selected = cursor_path
        .as_deref()
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
                header,
            )
        });
    let raw_selected = selected_path
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
                header,
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
        cursor_path,
        selected,
        raw_selected,
        usual_so_far_tokens,
    }
}

/// Issue #704 rule 2: the Store's own table cursor for `selected_path`
/// -- itself, when it names a row the table already has (a store
/// with tokens of its own); else the nearest ancestor that is one;
/// else the nearest token-bearing leaf in plan order (`plan_order`,
/// the run's own rows, is what gives "nearest" a real meaning --
/// never the table's own tokens-descending order, which would pick
/// the biggest store in the run instead of the closest one). `None`
/// only when `selected_path` is absent from `plan_order` entirely
/// (an inferred decision's own synthetic station has no row of its
/// own) and the table is empty, which `build_store`'s own caller
/// falls back from to the table's first row.
pub fn store_cursor_path(
    selected_path: Option<&str>,
    table: &[StoreTableRow],
    plan_order: &[String],
) -> Option<String> {
    let selected_path = selected_path?;
    let mut candidate = selected_path;
    loop {
        if table.iter().any(|r| r.path == candidate) {
            return Some(candidate.to_string());
        }
        match candidate.rsplit_once('.') {
            Some((parent, _)) => candidate = parent,
            None => break,
        }
    }
    nearest_token_bearing_path(selected_path, table, plan_order)
}

/// `store_cursor_path`'s own plan-order fallback: the table's own
/// path closest to `selected_path` by position in `plan_order`
/// (ties broken toward the earlier one) -- `None` when `selected_
/// path` isn't in `plan_order` at all, or no table row is either.
pub fn nearest_token_bearing_path(
    selected_path: &str,
    table: &[StoreTableRow],
    plan_order: &[String],
) -> Option<String> {
    let table_paths: Vec<String> = table.iter().map(|r| r.path.clone()).collect();
    nearest_path_in_plan_order(selected_path, &table_paths, plan_order)
}

/// The path in `candidates` closest to `selected_path` by position
/// in `plan_order` (ties broken toward the earlier one) -- shared by
/// [`nearest_token_bearing_path`] and the Store's own `enter`/`
/// ↑↓` key handling, which has no [`StoreTableRow`] of its own to
/// read tokens off, only the table's own path list. `None` when
/// `selected_path` isn't in `plan_order` at all, or no candidate is
/// either.
pub fn nearest_path_in_plan_order(
    selected_path: &str,
    candidates: &[String],
    plan_order: &[String],
) -> Option<String> {
    let idx = plan_order.iter().position(|p| p == selected_path)?;
    candidates
        .iter()
        .filter_map(|path| {
            plan_order
                .iter()
                .position(|p| p == path)
                .map(|pos| (pos.abs_diff(idx), pos, path.clone()))
        })
        .min_by_key(|(dist, pos, _)| (*dist, *pos))
        .map(|(_, _, path)| path)
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
            tokens_sent_unknown: false,
            tokens_received_unknown: false,
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

    fn table_row(path: &str, tokens: u64) -> StoreTableRow {
        StoreTableRow {
            path: path.to_string(),
            label: path.to_string(),
            tokens_sent: tokens,
            tokens_received: 0,
            share_pct: 0.0,
            deviation_pct: None,
        }
    }

    /// Issue #704 rule 2: a selected effect with no tokens of its
    /// own resolves to the nearest ancestor that has some, never to
    /// nothing at all just because the leaf itself never held any.
    #[test]
    fn store_cursor_path_climbs_to_the_nearest_ancestor_with_tokens() {
        let table = vec![table_row("prime.gate", 10)];
        let plan_order = vec!["prime.gate".to_string(), "prime.gate.reply".to_string()];
        assert_eq!(
            store_cursor_path(Some("prime.gate.reply"), &table, &plan_order),
            Some("prime.gate".to_string())
        );
        // Already in the table: itself, not an ancestor.
        assert_eq!(
            store_cursor_path(Some("prime.gate"), &table, &plan_order),
            Some("prime.gate".to_string())
        );
        // Neither itself nor an ancestor is in `plan_order` at all:
        // no cursor.
        assert_eq!(
            store_cursor_path(Some("prime.other.leaf"), &table, &plan_order),
            None
        );
        assert_eq!(store_cursor_path(None, &table, &plan_order), None);
    }

    /// Issue #704 rule 2: with no ancestor in the table either,
    /// "nearest" means plan order, never the table's own
    /// tokens-descending order -- the bigger, farther store must
    /// lose to the smaller, closer one.
    #[test]
    fn store_cursor_path_falls_back_to_the_nearest_leaf_in_plan_order() {
        let table = vec![table_row("prime.big", 900), table_row("prime.small", 5)];
        let plan_order = vec![
            "prime.small".to_string(),
            "prime.mid".to_string(),
            "prime.big".to_string(),
        ];
        assert_eq!(
            store_cursor_path(Some("prime.mid"), &table, &plan_order),
            Some("prime.small".to_string())
        );
    }

    /// The frames' own geometry (issue #695): at 120x30 `kind`
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

    /// Issue #695's settled point 1: siblings alternate columns and
    /// rows by depth, not just once -- three levels deep, depth 2's
    /// own siblings sit in columns again, the same as depth 0's.
    #[test]
    fn layout_alternates_columns_and_rows_at_depth_two() {
        let tree = vec![container(
            "prime.a",
            "a",
            false,
            vec![container(
                "prime.a.b",
                "b",
                false,
                vec![
                    leaf("prime.a.b.c1", "c1", 10),
                    leaf("prime.a.b.c2", "c2", 10),
                ],
            )],
        )];
        let tiles = layout_store(&tree, 0, 0, 40, 20);
        let c1 = tiles.iter().find(|t| t.path == "prime.a.b.c1").unwrap();
        let c2 = tiles.iter().find(|t| t.path == "prime.a.b.c2").unwrap();
        assert_eq!(
            c1.rect.y, c2.rect.y,
            "depth 2 siblings sit in columns, sharing a row: {tiles:#?}"
        );
        assert_ne!(
            c1.rect.x, c2.rect.x,
            "depth 2 siblings sit in columns, each its own: {tiles:#?}"
        );
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
        assert!(
            more.rect.w >= MORE_TILE_MIN_WIDTH,
            "the More tile must never be apportioned down below its own minimum width, got {more:?}"
        );
        match &more.kind {
            StoreTileKind::More { count, tokens } => {
                assert_eq!(*count, 2);
                assert_eq!(*tokens, 2);
            }
            _ => unreachable!(),
        }
        assert!(tiles.iter().any(|t| t.path == "prime.big"));
    }

    /// Issue #695's rule 5: when every top-level store is too small
    /// for the full span (forty equal-weight stores at 80 columns,
    /// each getting under two columns on its own), the largest ones
    /// that fit keep a tile and the rest fold -- the diagram never
    /// goes blank because every tile apportioned to zero.
    #[test]
    fn when_every_top_level_store_is_too_small_the_largest_still_keep_tiles() {
        let tree: Vec<StoreNode> = (0..40)
            .map(|i| leaf(&format!("prime.n{i}"), &format!("n{i}"), 1))
            .collect();
        let tiles = layout_store(&tree, 0, 0, 80, 10);
        let more = tiles
            .iter()
            .find(|t| matches!(t.kind, StoreTileKind::More { .. }))
            .expect("too many equal stores for 80 columns must still fold most of them");
        assert!(more.rect.w >= MORE_TILE_MIN_WIDTH, "tiles were {tiles:#?}");
        let kept: Vec<&StoreTile> = tiles
            .iter()
            .filter(|t| !matches!(t.kind, StoreTileKind::More { .. }))
            .collect();
        assert!(
            !kept.is_empty(),
            "at least one store must keep its own tile"
        );
        for tile in &kept {
            assert!(tile.rect.w >= MIN_TILE_WIDTH, "tiles were {tiles:#?}");
        }
        if let StoreTileKind::More { count, .. } = more.kind {
            assert_eq!(count + kept.len(), 40);
        }
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
        let tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", true, None,
        );
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
        let tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", true, None,
        );
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
        let tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", true, None,
        );
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
    fn a_model_mode_decisions_own_tokens_become_a_check_leaf() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "gate": {"value": null, "meta": {
                    "created_at": "t0", "completed_at": "t1",
                    "tokens_sent_total": 20, "tokens_received_total": 4
                },
                    "reply": {"value": "y", "meta": {
                        "created_at": "t0", "completed_at": "t1",
                        "tokens_sent_total": 100, "tokens_received_total": 30
                    }}
                }
            }
        }));
        let mut kinds = BTreeMap::new();
        kinds.insert("prime.gate".to_string(), RowKind::If);
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", true, None,
        );
        let StoreNode::Container(gate) = &tree[0] else {
            panic!("expected gate to stay a container");
        };
        assert_eq!(
            gate.children.len(),
            2,
            "the gate's own tokens join `reply` as a second leaf"
        );
        let check = gate
            .children
            .iter()
            .find_map(|c| match c {
                StoreNode::Leaf(l) if l.label == "check" => Some(l),
                _ => None,
            })
            .expect("the decision's own tokens must draw as a `check` leaf");
        assert_eq!(check.path, "prime.gate");
        assert_eq!(check.tokens(), 24);
        let gate_tokens: u64 = gate.children.iter().map(StoreNode::tokens).sum();
        assert_eq!(
            gate_tokens,
            24 + 130,
            "the container's own area includes its own tokens"
        );
        // The check leaf is the container's own node, not a planned
        // child, so it must not inflate "<n>/<m> with tokens".
        assert_eq!(gate.children_with_tokens, 1);
        assert_eq!(gate.children_planned, 1);
    }

    /// Issue #695's rule 5: a 3-pass loop with only 2 passes
    /// started so far reads `2/3 with tokens`, from the plan's own
    /// `resolve_each_len` (the loop's static `each:` array length),
    /// not `2/2` from however many passes merely exist in state yet.
    #[test]
    fn children_planned_reads_the_loops_own_planned_pass_count_from_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let doc_path = dir.path().join("do.yml");
        std::fs::write(
            &doc_path,
            "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: drafts\n    type: loop\n    each: {in: input.topics, as: topic}\n    body:\n      - name: draft\n        type: prompt\n        template: \"{{topic}}\"\n",
        )
        .unwrap();
        let program = crate::plan::compile(&doc_path, None, &indexmap::IndexMap::new())
            .unwrap_or_else(|e| panic!("test document doesn't compile: {e}"));
        let plan = crate::plan::PlanTree::from_program(&program);

        let state = serde_json::json!({
            "input": {"topics": ["a", "b", "c"]},
            "prime": {"value": null, "meta": {"completed_at": null},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": null}},
                "iter_0": {"draft": {"value": "y", "meta": {
                    "created_at": "t0", "completed_at": "t1",
                    "tokens_sent_total": 100, "tokens_received_total": 30
                }}},
                "iter_1": {"draft": {"value": "y", "meta": {
                    "created_at": "t0", "completed_at": "t1",
                    "tokens_sent_total": 10, "tokens_received_total": 3
                }}}
            }
        });
        // Deliberately not nested under `prime.drafts` -- a loop's own
        // passes land as `prime.drafts.iter_N` in real state, but this
        // test only needs `direct_children_of("prime.drafts", ..)` to
        // disagree with `resolve_each_len`'s own count, not a
        // byte-exact replica of the engine's own layout.
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                    "iter_0": {"draft": {"value": "y", "meta": {
                        "created_at": "t0", "completed_at": "t1",
                        "tokens_sent_total": 100, "tokens_received_total": 30
                    }}},
                    "iter_1": {"draft": {"value": "y", "meta": {
                        "created_at": "t0", "completed_at": "t1",
                        "tokens_sent_total": 10, "tokens_received_total": 3
                    }}}
                }
            }
        }));
        let mut kinds = BTreeMap::new();
        kinds.insert("prime.drafts".to_string(), RowKind::Loop);
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree(
            "prime",
            &flat,
            &kinds,
            &plan,
            &header,
            &model,
            "t1",
            false,
            Some(&state),
        );
        let StoreNode::Container(drafts) = &tree[0] else {
            panic!("expected drafts to stay a container");
        };
        assert_eq!(drafts.children_with_tokens, 2);
        assert_eq!(drafts.children_planned, 3);
    }

    #[test]
    fn a_nested_loop_builds_a_container_inside_a_container() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "outer": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"},
                    "iter_0": {
                        "inner": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"},
                            "iter_0": {
                                "draft": {"value": "y", "meta": {
                                    "created_at": "t0", "completed_at": "t1",
                                    "tokens_sent_total": 10, "tokens_received_total": 2
                                }},
                                "critique": {"value": "y", "meta": {
                                    "created_at": "t0", "completed_at": "t1",
                                    "tokens_sent_total": 5, "tokens_received_total": 1
                                }}
                            }
                        }
                    }
                }
            }
        }));
        let mut kinds = BTreeMap::new();
        kinds.insert("prime.outer".to_string(), RowKind::Loop);
        kinds.insert("prime.outer.iter_0.inner".to_string(), RowKind::Loop);
        let plan = crate::plan::PlanTree::empty();
        let header = empty_header();
        let model = RunModel::new();
        let tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", true, None,
        );
        let StoreNode::Container(outer) = &tree[0] else {
            panic!("expected outer to stay a container");
        };
        assert_eq!(outer.label, "outer");
        assert_eq!(outer.children.len(), 1);
        // Outer's own single pass holds exactly one structural child
        // (`inner`), but `inner` itself carries two token-bearing
        // leaves, so the pass never folds: it stays a container
        // around `inner`'s own container.
        let StoreNode::Container(outer_pass) = &outer.children[0] else {
            panic!("outer's own pass must stay a container around `inner`");
        };
        assert_eq!(outer_pass.label, "iter_0");
        assert_eq!(outer_pass.children.len(), 1);
        let StoreNode::Container(inner) = &outer_pass.children[0] else {
            panic!("expected the inner loop to be its own container");
        };
        assert_eq!(inner.label, "inner");
        assert_eq!(inner.children.len(), 1);
        let StoreNode::Container(pass) = &inner.children[0] else {
            panic!("a pass with two token-bearing steps must stay a container");
        };
        assert_eq!(pass.label, "iter_0");
        assert_eq!(pass.children.len(), 2);
        let outer_tokens: u64 = outer.children.iter().map(StoreNode::tokens).sum();
        assert_eq!(outer_tokens, 18);
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
            None,
        );
        assert_eq!(tree.len(), 1);
        let StoreNode::Leaf(ask) = &tree[0] else {
            panic!("expected a live leaf tile for the running prompt");
        };
        assert!(ask.live);
        assert_eq!(ask.tokens_sent, 40);
    }

    /// Issue #695's rule 13: an agent tool's own `meta.live` (no
    /// `adapter` key, told apart from a streamed prompt's by
    /// [`crate::model::AgentLive::from_value`]) gets a faint tile
    /// from its own running totals too, the same as a streamed
    /// prompt's estimate.
    #[test]
    fn an_agent_sessions_meta_live_gets_a_tile_from_its_own_running_totals() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "code": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z",
                    "provider": "agent",
                    "live": {
                        "engine": "claude-code",
                        "updated_at": "2026-01-01T00:00:03Z",
                        "tokens_sent": 50,
                        "tokens_received": 20
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
            None,
        );
        assert_eq!(tree.len(), 1);
        let StoreNode::Leaf(code) = &tree[0] else {
            panic!("expected a live leaf tile for the running agent session");
        };
        assert!(code.live);
        assert_eq!(code.tokens_sent, 50);
        assert_eq!(code.tokens_received, 20);
    }

    /// Issue #702's rule 1: a stale leftover `meta.live` (an
    /// interrupted agent session, `updated_at` more than 5 seconds
    /// behind `now`) still gets a tile from its own last known
    /// figures, just drawn normally rather than faint -- never
    /// dropped the way `a_running_step_with_no_live_figures_gets_no_
    /// tile` below is.
    #[test]
    fn a_stale_leftover_agent_live_gets_a_tile_not_flagged_live() {
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "code": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z",
                    "provider": "agent",
                    "live": {
                        "engine": "pi",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "tokens_sent": 2100,
                        "tokens_received": 40
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
            "2026-01-01T00:00:30Z",
            false,
            None,
        );
        assert_eq!(tree.len(), 1);
        let StoreNode::Leaf(code) = &tree[0] else {
            panic!("expected a tile for the stale leftover agent session");
        };
        assert!(!code.live);
        assert_eq!(code.tokens_sent, 2100);
        assert_eq!(code.tokens_received, 40);
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
            None,
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

        let running_tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", false, None,
        );
        let StoreNode::Container(drafts) = &running_tree[0] else {
            panic!("expected a container");
        };
        assert!(drafts.running, "a running container carries the marker");

        let ended_tree = build_store_tree(
            "prime", &flat, &kinds, &plan, &header, &model, "t1", true, None,
        );
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
    fn table_row_deviation_is_none_without_history() {
        let tree = vec![leaf("prime.kind", "kind", 215)];
        let table = build_store_table(&tree, "prime", 215);
        assert_eq!(table[0].deviation_pct, None);
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
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                    "iter_0": {"draft": {"value": "y", "meta": {
                        "created_at": "t0", "completed_at": "t1",
                        "tokens_sent_total": 100, "tokens_received_total": 30
                    }}},
                    "iter_1": {"draft": {"value": "y", "meta": {
                        "created_at": "t0", "completed_at": "t1",
                        "tokens_sent_total": 100, "tokens_received_total": 31
                    }}}
                }
            }
        }));
        let header = empty_header();
        let table = build_store_table(&tree, "prime", 261);
        let readings = build_store_readings(&table, &flat, &header, &rows);
        assert_eq!(readings.loop_progress, Some(("drafts".to_string(), 2, 3)));
    }

    /// Issue #695: a running loop whose passes have no tokens yet
    /// still gets a Loop line (`drafts · 0 of 3 passes have tokens`)
    /// -- read from state, not from the tree [`build_store_tree`]
    /// draws no container for at all while nothing in it has tokens.
    #[test]
    fn readings_report_zero_of_m_for_a_running_loop_with_no_tokens_yet() {
        let tree: Vec<StoreNode> = Vec::new();
        let flat: BTreeMap<String, NodeMeta> = crate::model::flatten_state(&serde_json::json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "drafts": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
            }
        }));
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
                done: 0,
                total: Some(3),
                eta_s: None,
            }),
            dim: None,
            has_children: true,
        }];
        let header = empty_header();
        let table = build_store_table(&tree, "prime", 0);
        let readings = build_store_readings(&table, &flat, &header, &rows);
        assert_eq!(readings.loop_progress, Some(("drafts".to_string(), 0, 3)));
    }

    /// Issue #704 rule 4: the full-screen step reads the exact
    /// selected effect (`raw_selected`), never the ancestor `[`/`]`
    /// substitutes for the table's own cursor/highlight (`selected`)
    /// when the effect has no tokens of its own -- stepping to a
    /// token-less tool must show that tool's own step, not whichever
    /// store it happens to be nested under.
    #[test]
    fn raw_selected_is_the_exact_effect_never_the_cursors_own_nearest_ancestor() {
        let plan = crate::plan::PlanTree::empty();
        let mut model = RunModel::new();
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z",
                "adapter": "scripted", "model": "m",
                "tokens_sent_total": 20, "tokens_received_total": 15
            }},
            "check": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:01Z", "completed_at": "2026-01-01T00:00:02Z"
            }}
        }});
        let rendered = crate::render::build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            crate::model::ProcessState::exited(false),
            2.0,
            "2026-01-01T00:00:02Z",
            &[],
        );
        let flat = crate::model::flatten_state(&state);
        let kinds: BTreeMap<String, RowKind> = BTreeMap::new();
        let screen = build_store(
            &rendered.rows,
            &rendered.header,
            &flat,
            &kinds,
            &plan,
            &model,
            Some(&state),
            "prime",
            "2026-01-01T00:00:02Z",
            Some("prime.check"),
        );
        assert_ne!(
            screen.selected.as_ref().map(|s| s.path.as_str()),
            Some("prime.check"),
            "a token-less step's own cursor climbs to an ancestor, the same as ever"
        );
        assert_eq!(
            screen.raw_selected.as_ref().map(|s| s.path.as_str()),
            Some("prime.check"),
            "but the full-screen step still reads the exact one"
        );
    }

    #[test]
    fn readings_absent_without_any_loop() {
        let tree = vec![leaf("prime.kind", "kind", 10)];
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let header = empty_header();
        let table = build_store_table(&tree, "prime", 10);
        let readings = build_store_readings(&table, &flat, &header, &[]);
        assert_eq!(readings.loop_progress, None);
    }

    /// Issue #702's rule 4: the Largest Stores table's own share
    /// never exceeds 100% while an agent session is running, even
    /// though its own live tokens aren't in `runtime.last_run.
    /// totals` yet -- the exact `19000%` this issue's own bug report
    /// found once the denominator didn't count a running step's own
    /// live estimate at all.
    #[test]
    fn the_store_table_share_never_exceeds_100_percent_while_an_agent_runs() {
        let plan = crate::plan::PlanTree::empty();
        let mut model = RunModel::new();
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z",
                "adapter": "scripted", "model": "m",
                "tokens_sent_total": 20, "tokens_received_total": 15
            }},
            "fixer": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:01Z", "completed_at": null, "provider": "agent",
                "live": {"engine": "pi", "updated_at": "2026-01-01T00:00:14Z",
                    "tokens_sent": 6650, "tokens_received": 0}
            }}
        }});
        let rendered = crate::render::build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            crate::model::ProcessState::Running,
            14.0,
            "2026-01-01T00:00:14Z",
            &[],
        );
        let flat = crate::model::flatten_state(&state);
        let kinds: BTreeMap<String, RowKind> = BTreeMap::new();
        let screen = build_store(
            &rendered.rows,
            &rendered.header,
            &flat,
            &kinds,
            &plan,
            &model,
            Some(&state),
            "prime",
            "2026-01-01T00:00:14Z",
            None,
        );
        assert!(!screen.table.is_empty());
        for row in &screen.table {
            assert!(
                row.share_pct <= 100.0,
                "{}: share {} exceeds 100%",
                row.label,
                row.share_pct
            );
        }
        let fixer = screen
            .table
            .iter()
            .find(|r| r.label.contains("fixer"))
            .expect("a fixer row in the table");
        assert!(fixer.share_pct > 99.0 && fixer.share_pct <= 100.0);
    }

    /// Issue #695: `build_store` over 2,000 flat, token-bearing
    /// leaves stays well under the redraw interval, the same bound
    /// `render::build`'s own 2,000-node test polices -- catches an
    /// accidental quadratic scan in `direct_children_of`/
    /// `sort_in_plan_order`/the table's own sort, not a single
    /// fixed budget.
    #[test]
    fn build_store_over_2000_leaves_stays_well_under_the_redraw_interval() {
        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        prime.insert(
            "meta".to_string(),
            serde_json::json!({"completed_at": null}),
        );
        for i in 0..2000 {
            prime.insert(
                format!("n{i}"),
                serde_json::json!({
                    "value": format!("ok{i}"),
                    "meta": {
                        "created_at": format!("t{i}"),
                        "completed_at": format!("t{}", i + 1),
                        "adapter": "scripted",
                        "tokens_sent_total": 1,
                        "tokens_received_total": 2
                    }
                }),
            );
        }
        let state = serde_json::json!({"prime": serde_json::Value::Object(prime)});
        let flat = crate::model::flatten_state(&state);
        let kinds: BTreeMap<String, RowKind> = BTreeMap::new();
        // A real, compiled plan with all 2,000 top-level effects named
        // -- `PlanTree::empty()` would never exercise `sort_in_plan_
        // order`'s own `all_paths().iter().position` scan per child at
        // all, since every lookup against an empty plan short-circuits
        // to `None` immediately.
        let dir = tempfile::tempdir().unwrap();
        let doc_path = dir.path().join("do.yml");
        let mut doc = String::from("adapter: scripted\nmodel: scripted-model\neffects:\n");
        for i in 0..2000 {
            doc.push_str(&format!(
                "  - name: n{i}\n    type: tool\n    provider: shell\n    params: {{command: \"true\", allowed_commands: [\"true\"]}}\n"
            ));
        }
        std::fs::write(&doc_path, doc).unwrap();
        let program = crate::plan::compile(&doc_path, None, &indexmap::IndexMap::new())
            .unwrap_or_else(|e| panic!("test document doesn't compile: {e}"));
        let plan = crate::plan::PlanTree::from_program(&program);
        let header = empty_header();
        let model = RunModel::new();
        let start = std::time::Instant::now();
        let screen = build_store(
            &[],
            &header,
            &flat,
            &kinds,
            &plan,
            &model,
            Some(&state),
            "prime",
            "t2001",
            None,
        );
        let elapsed = start.elapsed();
        assert_eq!(screen.tree.len(), 2000);
        assert_eq!(screen.table.len(), 2000);
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "building the Store from 2,000 leaves took {elapsed:?}"
        );
    }
}
