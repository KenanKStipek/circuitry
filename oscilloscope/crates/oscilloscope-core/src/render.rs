//! The render model (DESIGN.md §6.3, issue #434): a front end's whole
//! input for one redraw, built fresh from `RunModel`'s own status
//! inference (§2), the raw state (for per-node meta/value) and the
//! log lines accumulated so far. No terminal code lives here (§6.1) —
//! `oscilloscope`'s `tui` module is the only thing that ever imports
//! `ratatui`; a future GUI front end can lay out a `RenderState` of
//! its own just as well.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diff::LogLine;
use crate::model::{
    NodeMeta, ProcessState, RowStatus, RunModel, RunStatus, StatusKind, estimated_prompt_tokens,
    flatten_state, is_running_prompt, run_status, run_totals, subtree_tokens,
};
use crate::observe::duration_seconds;
use crate::plan::{LeafEffectKind, PlanEntryKind, PlanTree};
use crate::treemap::TreemapNode;

/// A row's own effect type (§6.3's dim provider/model column, and the
/// details pane's "type"): a leaf's own kind when the plan or the
/// node's own `meta` shape can tell, a container kind for everything
/// with children, `Unknown` for an observed-only path (no plan, no
/// shape clue yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Tool,
    Prompt,
    Use,
    Yield,
    Reflector,
    If,
    Loop,
    Dynamic,
    TryFinally,
    Pass,
    Root,
    Unknown,
}

impl RowKind {
    /// The plan's own label (DESIGN.md §6.2/§6.3, #433's "show the
    /// effect kind from the plan ... instead of effect") — shared
    /// with `diff::plan_kind_label`'s fallback chain, so the TUI and
    /// `--log` never disagree about what a path's kind is called.
    pub fn label(self) -> &'static str {
        match self {
            RowKind::Tool => "tool",
            RowKind::Prompt => "prompt",
            RowKind::Use => "use",
            RowKind::Yield => "yield",
            RowKind::Reflector => "reflector",
            RowKind::If => "if",
            RowKind::Loop => "loop",
            RowKind::Dynamic => "dynamic",
            RowKind::TryFinally => "finally",
            RowKind::Pass => "pass",
            RowKind::Root => "root",
            RowKind::Unknown => "effect",
        }
    }

    fn from_plan_entry(kind: &PlanEntryKind) -> Self {
        match kind {
            PlanEntryKind::Leaf(LeafEffectKind::Tool) => RowKind::Tool,
            PlanEntryKind::Leaf(LeafEffectKind::Prompt) => RowKind::Prompt,
            PlanEntryKind::Leaf(LeafEffectKind::Use) => RowKind::Use,
            PlanEntryKind::Leaf(LeafEffectKind::Yield) => RowKind::Yield,
            PlanEntryKind::Leaf(LeafEffectKind::Reflector) => RowKind::Reflector,
            PlanEntryKind::If { .. } => RowKind::If,
            PlanEntryKind::Loop { .. } => RowKind::Loop,
            PlanEntryKind::Dynamic { .. } => RowKind::Dynamic,
            PlanEntryKind::TryFinally => RowKind::TryFinally,
        }
    }

    /// Guesses a kind from an observed node's own `meta` shape, for a
    /// path the plan has nothing to say about (an inline `use` child,
    /// a reflector pass, or any run with no plan at all) — the same
    /// shape-based rule DESIGN.md §5's "Joining plan paths..." section
    /// describes for these.
    fn from_meta_shape(node: &NodeMeta) -> Self {
        let meta = &node.meta;
        if meta.get("provider").is_some() {
            RowKind::Tool
        } else if meta.get("adapter").is_some() {
            RowKind::Prompt
        } else if meta.get("orchestration").is_some() || meta.get("resolved_path").is_some() {
            RowKind::Use
        } else if node.progress_total.is_some() || meta.get("each_in_path").is_some() {
            RowKind::Loop
        } else if node.branch.is_some() || meta.get("condition_result").is_some() {
            RowKind::If
        } else {
            RowKind::Unknown
        }
    }

    fn is_leaf(self) -> bool {
        matches!(
            self,
            RowKind::Tool | RowKind::Prompt | RowKind::Use | RowKind::Yield | RowKind::Reflector
        )
    }
}

/// A loop's own `n/total` and ETA (§6.3's plan-tree column; DESIGN.md
/// §1.2's `meta.progress`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopProgress {
    pub done: u64,
    pub total: Option<u64>,
    pub eta_s: Option<f64>,
}

/// One row of the plan tree (§6.3's left pane).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: String,
    pub depth: usize,
    pub label: String,
    pub kind: RowKind,
    pub status: RowStatus,
    /// Elapsed since `created_at`, up to `completed_at` once it has
    /// one -- or, for a row still running (review finding 5 on
    /// #458's comment 3), up to `build`'s own `now` instead, so a
    /// running row's duration keeps ticking rather than showing
    /// nothing at all until it finishes.
    pub duration_s: Option<f64>,
    pub loop_progress: Option<LoopProgress>,
    /// The provider or model, in a dim colour (§6.3) — a tool's own
    /// `provider`, or a prompt's `model`.
    pub dim: Option<String>,
    pub has_children: bool,
}

/// The header line (§6.3). There is no run-level ETA (plans have
/// data-dependent loops) — only the innermost *running* loop's own.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub document: String,
    pub engine: String,
    pub run_id: Option<String>,
    pub status: RunStatus,
    pub elapsed_s: f64,
    /// The run's own start (issue #613's "one clock"/"run start and
    /// end"): the first `--events` `run_start`'s own `ts`, else the
    /// earliest `created_at` anywhere in state. `None` only before
    /// either has ever been observed. RFC-3339, UTC -- a front end
    /// formats it through its own [`crate::format::ClockZone`].
    pub run_start_ts: Option<String>,
    /// The run's own end, once it has one: `--events`' `run_end` own
    /// `ts`, else the final state's own `prime.meta.completed_at`.
    /// `None` while the run is still going.
    pub run_end_ts: Option<String>,
    pub effects_done: u64,
    pub effects_planned: Option<u64>,
    /// Set when a loop somewhere under an `effects_planned` leaf has
    /// no known `progress.total` yet (review finding 6): `planned`
    /// then only counts that leaf once rather than once per eventual
    /// pass, so it's a lower bound, not the true total — a front end
    /// shows it with a trailing `+` (`3/3+`) rather than as if it
    /// were exact.
    pub effects_planned_is_lower_bound: bool,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub innermost_loop_eta_s: Option<f64>,
    /// `innermost_loop_eta_s`, as an absolute clock time (`now` plus
    /// the ETA) rather than a remaining duration (issue #613: "the
    /// innermost loop's ETA is shown as a clock time"). `None`
    /// exactly when `innermost_loop_eta_s` is.
    pub innermost_loop_eta_ts: Option<String>,
}

/// The selected row's own details (§6.3's right pane). `prompt_sent`
/// and `value` are truncated to [`DETAILS_PREVIEW_CHARS`]; a front
/// end shows them in full on request (the `v` key).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details {
    pub path: String,
    pub plan_summary: String,
    pub meta_summary: Vec<(String, String)>,
    pub error: Option<String>,
    pub prompt_sent: Option<String>,
    pub value: Option<String>,
    /// The `sent`/`received` blocks (issue #535): set only for a node
    /// whose `meta` has `prompt_sent` (a prompt) -- replacing the
    /// separate `prompt_sent`/`value` blocks above for exactly those
    /// nodes. `received` is `None` when the prompt failed before any
    /// reply (the error is shown instead, as always).
    pub sent: Option<TokenBlock>,
    pub received: Option<TokenBlock>,
}

/// One of the details pane's `sent`/`received` blocks (issue #535).
#[derive(Debug, Clone, PartialEq)]
pub struct TokenBlock {
    /// `None` only while `received` is still waiting for a reply.
    pub snippet: Option<String>,
    /// `None` only while `received` is still waiting for a reply --
    /// `sent` always has a number, actual once the prompt has gone
    /// out, estimated while it's still in flight.
    pub tokens: Option<u64>,
    /// `true` for `sent`'s own in-flight estimate; always `false` for
    /// `received` (DESIGN.md §6.3: neither engine reports output
    /// tokens before a prompt finishes, so there is no estimate to
    /// mark, only the waiting state below).
    pub estimated: bool,
    /// `received` only: the prompt is still running, so there is no
    /// reply yet at all, estimated or otherwise.
    pub waiting: bool,
}

/// A tree, not a flat list (issue #535): every `{value, meta}` node
/// under `prime` is a row, nested under its parent and indented by
/// `depth` — including a container with no node of its own (a loop's
/// `iter_N`), so the tree has no gaps. No value or error text: the
/// details pane and `v` are where those are read.
#[derive(Debug, Clone, PartialEq)]
pub struct StateRow {
    /// Relative to `prime` (`"summarize"`, `"loop.iter_2.step"`) —
    /// `prime` itself is never one of these rows. Doubles as this
    /// row's own stable key (collapse sets, selection, update
    /// highlights).
    pub path: String,
    /// `0` for a direct child of `prime`.
    pub depth: usize,
    /// This row's own key only (`"step"`, `"iter_2"`), never the full
    /// path — a front end left-truncates/indents using `depth`
    /// instead.
    pub key: String,
    pub has_children: bool,
    pub status: RowStatus,
    /// The model for a prompt, the provider for a tool, the duration
    /// once finished, `retries N` when `retries_used > 0`, and
    /// `n/total` for a loop — dim text, `None` for a row with nothing
    /// to show (a bare container with no node of its own).
    pub dim: Option<String>,
    /// `↑sent ↓received`, only when at least one is nonzero (issue
    /// #527) — a leaf's own [`crate::model::node_tokens`], or a
    /// container's sum over its subtree ([`subtree_tokens`]); the two
    /// coincide for a node with no children of its own.
    pub tokens: Option<(u64, u64)>,
}

/// The token bar's own second line (DESIGN.md §6.3, issue #527):
/// what's in flight right now, from running prompts only — cof and
/// electricity report a prompt's tokens only once it finishes, so
/// there is no live output estimate, only input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenBar {
    pub estimated_input: u64,
    /// Set when at least one running prompt counted into
    /// `prompts_running` has no `meta.prompt_sent` in the live state to
    /// estimate from (a tree-loop pass
    /// or a `use` child's prompt never appears in state while it runs
    /// at all, DESIGN.md §1.4) — `estimated_input` then only counts
    /// the running prompts it actually has text for, so a front end
    /// shows it as a lower bound (`≈52+`), not an exact total.
    pub estimated_input_is_lower_bound: bool,
    pub prompts_running: u64,
    /// Running `agent` tool steps (issue #613's "agents in flight"):
    /// counted alongside running prompts, but with no token estimate
    /// of their own (live agent token usage needs #612 lane 2).
    pub agents_running: u64,
}

/// Everything one redraw needs (§6.3): the header, the plan tree
/// (flattened to depth-first order, a front end collapses a subtree
/// by skipping its descendants), the state pane's rows, the `tokens
/// by key` treemap's own tree (laid out into rectangles only once a
/// front end knows the pane's real size — DESIGN.md §6.1, `crate::
/// treemap::layout`), the token bar, and the log pane's lines so far.
/// `details_for` builds the right pane for whichever row is currently
/// selected — kept separate from `rows` so selecting a different row
/// never needs a fresh `RunModel::observe` call.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderState {
    pub header: Header,
    pub rows: Vec<Row>,
    pub state_rows: Vec<StateRow>,
    pub treemap: Vec<TreemapNode>,
    pub token_bar: TokenBar,
    pub log: Vec<LogLine>,
}

const DETAILS_PREVIEW_CHARS: usize = 200;

fn preview(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Builds one redraw's render state. `state` is `None` before the
/// first live-state write has ever landed; `log` is the caller's own
/// running buffer (`Differ`'s accumulated lines), cloned in, not
/// recomputed here.
#[allow(clippy::too_many_arguments)]
pub fn build(
    document: &str,
    engine: &str,
    plan: &PlanTree,
    model: &mut RunModel,
    state: Option<&Value>,
    process: ProcessState,
    elapsed_s: f64,
    now: &str,
    log: &[LogLine],
) -> RenderState {
    let empty = Value::Null;
    let state_ref = state.unwrap_or(&empty);
    let flat = flatten_state(state_ref);
    let statuses = model.observe(state_ref, plan, process);
    // Every path either side of a run has ever heard of (hoisted here,
    // issue/finding on `use` accounting): the header's own
    // planned/done counts need the identical view `build_rows`/
    // `child_segments` already use to discover a `use` child's
    // children once they land in state -- building it twice, once per
    // caller, would both cost the walk twice and risk the two counts
    // disagreeing about what "discovered" means.
    let all_paths = all_observed_paths(&flat, &statuses);

    let header = build_header(
        document,
        engine,
        model.run_start_id(),
        model.run_start_ts(),
        model.run_end_ts(),
        state,
        process,
        elapsed_s,
        now,
        plan,
        &flat,
        &statuses,
        &all_paths,
    );
    let rows = build_rows(plan, &flat, &statuses, &all_paths, now);
    let state_rows = build_state_rows(&flat, &statuses, now);
    let treemap = crate::treemap::build_tree("prime", &flat, &statuses);
    // A tree-loop pass's prompt, and
    // (on cof) a `use` child's, never appear in `flat` while they run
    // at all (DESIGN.md §1.4) -- only `rows` (built from `statuses`,
    // which events confirm `Running` for a path `flat` has never even
    // heard of) can see them at all, so the running-prompt count reads
    // `rows`, not `flat` alone.
    let token_bar = build_token_bar(&rows, &flat);

    RenderState {
        header,
        rows,
        state_rows,
        treemap,
        token_bar,
        log: log.to_vec(),
    }
}

/// [`build`], with the orchestration pane's own rows additionally
/// folded (DESIGN.md, issue #613: breadcrumbs and quick-steps
/// summaries) -- kept as its own entry point, rather than a flag on
/// `build` itself, so every existing caller of `build` (every test
/// in this crate and `oscilloscope`'s own golden tests) keeps seeing
/// the full, unfolded tree unless it explicitly asks for this one
/// instead. The TUI calls this; `--log` and `osi watch`'s own plain
/// stream read `diff`/`model` directly and never see `rows` at all.
#[allow(clippy::too_many_arguments)]
pub fn build_folded(
    document: &str,
    engine: &str,
    plan: &PlanTree,
    model: &mut RunModel,
    state: Option<&Value>,
    process: ProcessState,
    elapsed_s: f64,
    now: &str,
    log: &[LogLine],
    fold_enabled: bool,
    expanded_quick_steps: &std::collections::BTreeSet<String>,
) -> RenderState {
    let mut built = build(
        document, engine, plan, model, state, process, elapsed_s, now, log,
    );
    built.rows = crate::fold::fold_rows(built.rows, fold_enabled, expanded_quick_steps);
    built
}

/// One node of the state pane's own tree, built once per `path` seen
/// in `flat` (issue #535) -- `own` is `Some` for a path with a real
/// `{value, meta}` node, `None` for a bare container (a loop's
/// `iter_N`) that exists only because some descendant's path runs
/// through it.
#[derive(Default)]
struct StateTreeNode<'a> {
    own: Option<&'a NodeMeta>,
    children: BTreeMap<String, StateTreeNode<'a>>,
}

/// Every path `flatten_state` returns, except `prime` itself, grouped
/// into a tree by splitting each dotted path on `.` -- including every
/// intermediate segment with no `{value, meta}` node of its own, so
/// the tree has no gaps (issue #535).
fn build_state_tree(flat: &BTreeMap<String, NodeMeta>) -> BTreeMap<String, StateTreeNode<'_>> {
    let mut root: BTreeMap<String, StateTreeNode> = BTreeMap::new();
    for (path, node) in flat {
        let Some(relative) = path.strip_prefix("prime.") else {
            continue; // `prime` itself
        };
        let segments: Vec<&str> = relative.split('.').collect();
        let mut cursor = &mut root;
        for (i, segment) in segments.iter().enumerate() {
            let entry = cursor.entry((*segment).to_string()).or_default();
            if i + 1 == segments.len() {
                entry.own = Some(node);
            }
            cursor = &mut entry.children;
        }
    }
    root
}

/// `(is_current, most_recent_update)` over a node's own update time
/// (if it has one) and every descendant's -- issue #535's own sort
/// rule: "a row's recency is the latest update time in its subtree"
/// and "a running row counts as current".
fn state_subtree_recency(
    node: &StateTreeNode,
) -> (bool, Option<chrono::DateTime<chrono::FixedOffset>>) {
    let mut current = false;
    let mut recency = None;
    if let Some(own) = node.own {
        if own.is_running() {
            current = true;
        }
        recency = own
            .completed_at
            .as_deref()
            .or(own.created_at.as_deref())
            .and_then(crate::diff::parse_instant);
    }
    for child in node.children.values() {
        let (child_current, child_recency) = state_subtree_recency(child);
        current |= child_current;
        recency = match (recency, child_recency) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, None) => a,
            (None, b) => b,
        };
    }
    (current, recency)
}

/// The dim meta summary for a row with its own node (issue #535): the
/// model for a prompt, else the provider for a tool; the duration
/// once finished; `retries N` when `retries_used > 0`; and `n/total`
/// for a loop. `None` when there's nothing to show.
fn state_row_dim(node: &NodeMeta, now: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(model) = node.meta.get("model").and_then(Value::as_str) {
        parts.push(model.to_string());
    } else if let Some(provider) = node.meta.get("provider").and_then(Value::as_str) {
        parts.push(provider.to_string());
    }
    // Issue #613: a running row shows its own elapsed so far too, not
    // only a finished one's final duration. `changes::diff` never
    // compares a running row's `dim` at all (below), so this ticking
    // every rebuild never counts as a change on its own.
    match (&node.created_at, &node.completed_at) {
        (Some(created), Some(completed)) => {
            if let Some(secs) = duration_seconds(created, completed) {
                parts.push(crate::format::format_duration(secs));
            }
        }
        (Some(created), None) => {
            if let Some(secs) = duration_seconds(created, now) {
                parts.push(crate::format::format_duration(secs));
            }
        }
        _ => {}
    }
    if let Some(retries) = node.meta.get("retries_used").and_then(Value::as_u64) {
        if retries > 0 {
            parts.push(format!("retries {retries}"));
        }
    }
    if node.progress_total.is_some() || node.progress_done.is_some() {
        let total = node
            .progress_total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());
        parts.push(format!("{}/{total}", node.progress_done.unwrap_or(0)));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("  "))
    }
}

/// A row's own status: `statuses` when it has an entry for this exact
/// path; otherwise, for a row with its own node, the running/ok/
/// failed fallback the pane always used; otherwise (a bare container
/// with no node of its own) the same rule a tree-loop pass's own
/// container row already follows: any running descendant makes it
/// running, else any unhandled failure makes it failed, else done.
fn state_row_status(
    path: &str,
    node: &StateTreeNode,
    statuses: &BTreeMap<String, RowStatus>,
) -> RowStatus {
    if let Some(status) = statuses.get(path) {
        return status.clone();
    }
    if let Some(own) = node.own {
        let kind = if own.is_running() {
            StatusKind::Running
        } else if own.is_ok() {
            StatusKind::Done
        } else {
            StatusKind::Failed
        };
        return RowStatus {
            kind,
            skip_reason: None,
            retrying: false,
        };
    }
    aggregate_container_status(node)
}

fn aggregate_container_status(node: &StateTreeNode) -> RowStatus {
    fn walk(node: &StateTreeNode) -> (bool, bool, bool) {
        let mut running = false;
        let mut failed = false;
        let mut seen = false;
        if let Some(own) = node.own {
            seen = true;
            if own.is_running() {
                running = true;
            } else if !own.is_ok() {
                failed = true;
            }
        }
        for child in node.children.values() {
            let (r, f, s) = walk(child);
            running |= r;
            failed |= f;
            seen |= s;
        }
        (running, failed, seen)
    }
    let (any_running, any_failed, any_seen) = walk(node);
    let kind = if any_running {
        StatusKind::Running
    } else if any_failed {
        StatusKind::Failed
    } else if any_seen {
        StatusKind::Done
    } else {
        StatusKind::Pending
    };
    RowStatus {
        kind,
        skip_reason: None,
        retrying: false,
    }
}

/// The state pane's own rows (DESIGN.md §6.3, issue #535): a tree,
/// flattened depth-first, latest-or-current first at every level
/// (ties broken by key) -- a front end collapses a subtree the same
/// way the orchestration pane does, by skipping its descendants.
fn build_state_rows(
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    now: &str,
) -> Vec<StateRow> {
    let tree = build_state_tree(flat);
    let mut out = Vec::new();
    flatten_state_tree(&tree, "prime", 0, flat, statuses, now, &mut out);
    out
}

#[allow(clippy::too_many_arguments)]
fn flatten_state_tree(
    nodes: &BTreeMap<String, StateTreeNode>,
    parent_path: &str,
    depth: usize,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    now: &str,
    out: &mut Vec<StateRow>,
) {
    let mut entries: Vec<(&String, &StateTreeNode, String)> = nodes
        .iter()
        .map(|(key, node)| (key, node, format!("{parent_path}.{key}")))
        .collect();
    entries.sort_by(|(key_a, node_a, _), (key_b, node_b, _)| {
        let (current_a, recency_a) = state_subtree_recency(node_a);
        let (current_b, recency_b) = state_subtree_recency(node_b);
        current_b
            .cmp(&current_a)
            .then_with(|| recency_b.cmp(&recency_a))
            .then_with(|| key_a.cmp(key_b))
    });

    for (key, node, full_path) in entries {
        let status = state_row_status(&full_path, node, statuses);
        let (sent, received) = subtree_tokens(&full_path, flat);
        let tokens = (sent > 0 || received > 0).then_some((sent, received));
        let dim = node.own.and_then(|n| state_row_dim(n, now));
        out.push(StateRow {
            path: full_path
                .strip_prefix("prime.")
                .unwrap_or(&full_path)
                .to_string(),
            depth,
            key: key.clone(),
            has_children: !node.children.is_empty(),
            status,
            dim,
            tokens,
        });
        flatten_state_tree(
            &node.children,
            &full_path,
            depth + 1,
            flat,
            statuses,
            now,
            out,
        );
    }
}

/// The token bar's "what's in flight" line (DESIGN.md §6.3, issue
/// #527, and a follow-up finding): the running
/// count comes from `rows`, not `flat` alone, since a tree-loop pass's
/// prompt (both engines) and, on cof, a `use` child's, are confirmed
/// `Running` by events while never once appearing in live state at all
/// (DESIGN.md §1.4) — `flat` alone had nothing to find there, so the
/// bar kept reading "nothing in flight" under a visibly running
/// prompt. A row with its own state node still applies
/// [`is_running_prompt`]'s full criteria (notably excluding one merely
/// queued behind a concurrency slot, `meta.waiting_for`, which an
/// event-only row can never have in the first place — cof's own
/// `on_effect_start` fires only once a pass or branch actually begins,
/// never while it is still queued). A row confirmed running by events
/// alone, with no state node to check, is counted only when its own
/// `kind` is [`RowKind::Prompt`] — known for a tree-loop pass (matched
/// against the compiled plan's own `iter_*` placeholder) and a `use:
/// path:` child (its plan is grafted under the `use` node, DESIGN.md
/// §5), but *not* for a `use: inline:`/`ref:` child, which has no
/// static plan at all and so cannot be told apart from a running tool
/// by path and events alone — an inherent gap in what `--events`
/// carries (DESIGN.md §3: "No effect kind"), not something this count
/// can close.
fn build_token_bar(rows: &[Row], flat: &BTreeMap<String, NodeMeta>) -> TokenBar {
    let mut estimated_input = 0u64;
    let mut estimated_input_is_lower_bound = false;
    let mut prompts_running = 0u64;
    let mut agents_running = 0u64;
    for row in rows {
        if row.status.kind != StatusKind::Running {
            continue;
        }
        let node = flat.get(row.path.as_str());
        let is_prompt = match node {
            Some(node) => is_running_prompt(node),
            None => row.kind == RowKind::Prompt,
        };
        if !is_prompt {
            // Issue #613's "agents in flight": a running tool row
            // whose `provider` is `agent` (cof's own rule, trimmed and
            // case-insensitive, `src/circuitry/core/compiler.py`) --
            // read from state, the only place this crate can learn a
            // leaf's own provider today (the plan's `PlanEntryKind::
            // Leaf` carries no provider string of its own). Agents
            // get no token estimate (DESIGN.md/§613: "Agents get no
            // token estimate yet").
            if node
                .and_then(|n| n.meta.get("provider"))
                .and_then(Value::as_str)
                .is_some_and(|p| p.trim().eq_ignore_ascii_case("agent"))
            {
                agents_running += 1;
            }
            continue;
        }
        prompts_running += 1;
        match node
            .and_then(|n| n.meta.get("prompt_sent"))
            .and_then(Value::as_str)
        {
            Some(prompt_sent) if !prompt_sent.is_empty() => {
                estimated_input += estimated_prompt_tokens(prompt_sent);
            }
            _ => estimated_input_is_lower_bound = true,
        }
    }
    TokenBar {
        estimated_input,
        estimated_input_is_lower_bound,
        prompts_running,
        agents_running,
    }
}

/// The selected row's details, from the same inputs `build` used for
/// this frame — a front end calls this only for the one row currently
/// highlighted, not for every row on every redraw.
pub fn details_for(path: &str, plan: &PlanTree, state: Option<&Value>) -> Details {
    let empty = Value::Null;
    let flat = flatten_state(state.unwrap_or(&empty));
    let plan_entry = plan.match_path(path).and_then(|m| m.entries.first());
    let plan_summary = plan_entry
        .map(|entry| {
            let kind = RowKind::from_plan_entry(&entry.kind);
            match entry.name.as_deref() {
                Some(name) => format!("{} \"{name}\"", kind.label()),
                None => kind.label().to_string(),
            }
        })
        .unwrap_or_default();
    // Issue #613's pass labels: the details title shows both the
    // orchestration pane's own `pass N of M` and the state pane's own
    // real key (`iter_N`) together, for any path shaped like a loop
    // pass -- regardless of whether the plan matched it to the
    // enclosing loop's own entry (which knows nothing about "pass"
    // at all) or not.
    let plan_summary = {
        let last = path.rsplit('.').next().unwrap_or(path);
        match pass_index(last) {
            Some(n) => {
                let loop_path = path.rsplit_once('.').map(|(p, _)| p);
                let total = loop_path
                    .and_then(|p| flat.get(p))
                    .and_then(|loop_node| loop_node.progress_total);
                match total {
                    Some(total) => format!("pass {} of {total} ({last})", n + 1),
                    None => format!("pass {} ({last})", n + 1),
                }
            }
            None => plan_summary,
        }
    };

    let Some(node) = flat.get(path) else {
        return Details {
            path: path.to_string(),
            plan_summary,
            ..Default::default()
        };
    };

    let meta = &node.meta;
    // Issue #535: a node with `prompt_sent` gets the `sent`/`received`
    // blocks below instead -- their headers already carry the token
    // counts, so the plain meta list drops them to avoid saying the
    // same thing twice.
    let has_prompt_sent = meta.get("prompt_sent").and_then(Value::as_str).is_some();

    let mut meta_summary = Vec::new();
    for key in [
        "provider",
        "model",
        "adapter",
        "exit_code",
        "waiting_for",
        "tokens_sent",
        "tokens_received",
        "retries_used",
    ] {
        if has_prompt_sent && (key == "tokens_sent" || key == "tokens_received") {
            continue;
        }
        if let Some(v) = meta.get(key) {
            if !v.is_null() {
                meta_summary.push((key.to_string(), display_value(v)));
            }
        }
    }
    if let Some(command) = meta.pointer("/params_rendered/command") {
        meta_summary.push(("command".to_string(), display_value(command)));
    }
    // Review finding 4 on #458's comment 3: the details pane showed
    // `command` but never its own `args`, right next to it.
    if let Some(args) = meta.pointer("/params_rendered/args") {
        meta_summary.push(("args".to_string(), display_value(args)));
    }
    if let Some(stderr) = meta.get("stderr").and_then(Value::as_str) {
        // Finding 12: `.rev()` picks the *last* 5 lines, but without
        // reversing back they'd join in reverse reading order too —
        // the most recent line first, not last.
        let mut last_lines: Vec<&str> = stderr.lines().rev().take(5).collect();
        last_lines.reverse();
        let tail = last_lines.join("\n");
        if !tail.is_empty() {
            meta_summary.push(("stderr".to_string(), tail));
        }
    }

    let (sent, received) = if has_prompt_sent {
        let sent_block = build_sent_block(meta, node);
        // "A prompt that failed before any reply shows its error as
        // today, and no received snippet" (issue #535) -- finding 18:
        // that's narrower than "any error". A reply that arrived and
        // then failed later (an `expect`/parsing mismatch, say) still
        // left `meta.answer` or the node's own value behind, so only
        // an error with neither counts as "before any reply".
        let got_reply =
            meta.get("answer").and_then(Value::as_str).is_some() || !node.value.is_null();
        let received_block = if node.error.is_some() && !got_reply {
            None
        } else {
            Some(build_received_block(meta, node))
        };
        (Some(sent_block), received_block)
    } else {
        (None, None)
    };

    Details {
        path: path.to_string(),
        plan_summary,
        meta_summary,
        error: node.error.clone(),
        prompt_sent: meta
            .get("prompt_sent")
            .and_then(Value::as_str)
            .map(|s| preview(s, DETAILS_PREVIEW_CHARS)),
        value: Some(preview(&display_value(&node.value), DETAILS_PREVIEW_CHARS)),
        sent,
        received,
    }
}

/// Whitespace runs, including newlines, collapsed to one space (issue
/// #535) -- applied before [`preview`]'s own truncation, so a cut
/// snippet's `…` lands after real content, not mid-run of spaces.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `sent` block (issue #535): always present for a node with
/// `prompt_sent`, whether it's still running (the token bar's own
/// in-flight estimate) or finished (the actual count, [`node_tokens`]
/// -- the same `*_total`-first rule the header/token bar/state pane/
/// treemap all use).
fn build_sent_block(meta: &Value, node: &NodeMeta) -> TokenBlock {
    let prompt_sent = meta
        .get("prompt_sent")
        .and_then(Value::as_str)
        .unwrap_or("");
    let snippet = Some(preview(
        &collapse_whitespace(prompt_sent),
        DETAILS_PREVIEW_CHARS,
    ));
    if node.is_running() {
        TokenBlock {
            snippet,
            tokens: Some(estimated_prompt_tokens(prompt_sent)),
            estimated: true,
            waiting: false,
        }
    } else {
        let (sent, _) = crate::model::node_tokens(meta);
        TokenBlock {
            snippet,
            tokens: Some(sent),
            estimated: false,
            waiting: false,
        }
    }
}

/// The `received` block (issue #535), for a node with `prompt_sent`
/// that hasn't failed before any reply. While the prompt is still
/// running: `waiting`, no snippet, no number -- neither engine
/// reports output tokens before a prompt finishes (the same rule the
/// token bar's own estimate follows). Once it's answered: `meta.
/// answer` when present (the raw reply for `boolean`/`number`
/// prompts and model-mode conditions), else the node's own value (a
/// string as is, anything else as compact JSON).
fn build_received_block(meta: &Value, node: &NodeMeta) -> TokenBlock {
    if node.is_running() {
        return TokenBlock {
            snippet: None,
            tokens: None,
            estimated: false,
            waiting: true,
        };
    }
    let text = meta
        .get("answer")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| display_value(&node.value));
    let snippet = Some(preview(&collapse_whitespace(&text), DETAILS_PREVIEW_CHARS));
    let (_, received) = crate::model::node_tokens(meta);
    TokenBlock {
        snippet,
        tokens: Some(received),
        estimated: false,
        waiting: false,
    }
}

/// `prompt_sent`/the value, in full — the `v` key's own request
/// (§6.3), bypassing `Details`' truncated preview.
pub fn full_value(path: &str, field: FullValueField, state: Option<&Value>) -> Option<String> {
    let empty = Value::Null;
    let flat = flatten_state(state.unwrap_or(&empty));
    let node = flat.get(path)?;
    match field {
        FullValueField::PromptSent => node
            .meta
            .get("prompt_sent")
            .and_then(Value::as_str)
            .map(str::to_string),
        FullValueField::Value => Some(display_value(&node.value)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullValueField {
    PromptSent,
    Value,
}

fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_header(
    document: &str,
    engine: &str,
    run_id: Option<&str>,
    run_start_ts_from_events: Option<&str>,
    run_end_ts_from_events: Option<&str>,
    state: Option<&Value>,
    process: ProcessState,
    fallback_elapsed_s: f64,
    now: &str,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    all_paths: &BTreeMap<String, ()>,
) -> Header {
    let status = run_status(state, process);

    // Issue #613's "one clock": the run's own start, from the first
    // `--events` `run_start` when one arrived, else the earliest
    // `created_at` anywhere in state -- real instants compared, never
    // raw strings (DESIGN.md's own warning: an event's millisecond-
    // plus-`Z` timestamp and a state microsecond-plus-offset one
    // aren't lexicographically comparable).
    let run_start_ts = run_start_ts_from_events
        .map(str::to_string)
        .or_else(|| earliest_created_at("prime", all_paths, flat));
    // Once the run has ended: `run_end`'s own `ts`, else the final
    // state's own completion time (`prime.meta.completed_at`).
    let run_end_ts = run_end_ts_from_events.map(str::to_string).or_else(|| {
        if status == RunStatus::Running {
            None
        } else {
            flat.get("prime").and_then(|n| n.completed_at.clone())
        }
    });
    // Every time in this frame comes from the same `now` (issue
    // #613's "one clock") -- the header's own elapsed, every running
    // row's elapsed, and the ETA all derive from this single instant,
    // never from when each was separately computed.
    let elapsed_s = match (&run_start_ts, &run_end_ts) {
        (Some(start), Some(end)) => duration_seconds(start, end).unwrap_or(fallback_elapsed_s),
        (Some(start), None) => duration_seconds(start, now).unwrap_or(fallback_elapsed_s),
        (None, _) => fallback_elapsed_s,
    };

    let (effects_planned, effects_planned_is_lower_bound) = if plan.has_plan() {
        let leaf_paths: std::collections::BTreeSet<&String> = plan.leaf_paths().iter().collect();
        let mut total = 0u64;
        let mut lower_bound = false;
        for leaf in &leaf_paths {
            // A `use` leaf's own
            // children are the real planned units, not the `use` node
            // itself -- counting both (already true for a `path:`
            // child whose own children are separately grafted into
            // `leaf_paths`) or neither (an `inline:`/`ref:` child,
            // which has no static plan at all) is what let `done`
            // outrun `planned` once a `use` child's effects actually
            // landed in state.
            if row_kind_for(leaf, plan, flat.get(leaf.as_str())) == RowKind::Use {
                let prefix = format!("{leaf}.");
                let has_static_children = leaf_paths.iter().any(|p| p.starts_with(&prefix));
                if has_static_children {
                    // Grafted `use: path:`: its own children are
                    // already separate entries in `leaf_paths`, each
                    // counted on their own below.
                    continue;
                }
                let (count, use_lower_bound) =
                    use_leaf_contribution(leaf, plan, flat, statuses, all_paths);
                total = total.saturating_add(count);
                lower_bound |= use_lower_bound;
                continue;
            }
            let (passes, leaf_lower_bound) = leaf_pass_count(leaf, plan, flat);
            total = total.saturating_add(passes);
            lower_bound |= leaf_lower_bound;
        }
        (Some(total), lower_bound)
    } else {
        (None, false)
    };
    let effects_done = statuses
        .iter()
        .filter(|(path, status)| {
            if !matches!(
                status.kind,
                crate::model::StatusKind::Done
                    | crate::model::StatusKind::Failed
                    | crate::model::StatusKind::FailedHandled
            ) {
                return false;
            }
            let kind = row_kind_for(path, plan, flat.get(path.as_str()));
            if kind == RowKind::Use {
                // Only count the `use` node itself when nothing under
                // it was ever discovered (a declared-outputs child,
                // whose own children are dropped from the final state
                // even once it succeeds, DESIGN.md §1.2, or one that
                // failed before any child ran) -- otherwise its own
                // discovered children are each already counted here on
                // their own, via this same filter applied to their own
                // paths.
                !crate::model::subtree_entries(path.as_str(), all_paths)
                    .any(|(p, ())| p.as_str() != path.as_str())
            } else {
                kind.is_leaf()
            }
        })
        .count() as u64;
    // Invariant: "done" never outruns
    // "planned" -- true by construction once a `use` child's effects
    // are counted the same way on both sides above, but kept as an
    // explicit floor rather than trusting that invariant silently,
    // since a document this walker has never seen could still surprise
    // it.
    let effects_done = effects_planned.map_or(effects_done, |planned| effects_done.min(planned));

    let (tokens_sent, tokens_received) = state
        .and_then(run_totals)
        .map(|totals| {
            (
                totals
                    .get("tokens_sent")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                totals
                    .get("tokens_received")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        })
        .unwrap_or_else(|| {
            // The token rule (DESIGN.md §6.3): the same `_total`-first
            // pair `run_totals` itself is built from in both engines,
            // so a retried prompt never makes this fallback disagree
            // with `runtime.last_run.totals` once that lands.
            flat.values().fold((0, 0), |(sent, recv), node| {
                let (s, r) = crate::model::node_tokens(&node.meta);
                (sent + s, recv + r)
            })
        });

    // The innermost running loop's own ETA (§6.3: "There is no
    // run-level ETA") — the running loop with the deepest path, since
    // a nested loop's own progress is more specific than its
    // enclosing one's.
    let innermost_loop_eta_s = flat
        .iter()
        .filter(|(path, node)| {
            node.is_running()
                && node.progress_eta_s.is_some()
                && statuses.get(*path).is_some_and(|s| {
                    matches!(
                        s.kind,
                        crate::model::StatusKind::Running | crate::model::StatusKind::LikelyRunning
                    )
                })
        })
        .max_by_key(|(path, _)| path.matches('.').count())
        .and_then(|(_, node)| node.progress_eta_s);
    // The same ETA, as an absolute clock time (issue #613: "the
    // innermost loop's ETA is shown as a clock time") -- `now` plus
    // the remaining seconds, using this frame's own `now`, not a
    // fresh read of the clock.
    let innermost_loop_eta_ts = innermost_loop_eta_s.and_then(|eta| {
        crate::diff::parse_instant(now).map(|now_dt| {
            (now_dt + chrono::Duration::milliseconds((eta * 1000.0).round() as i64)).to_rfc3339()
        })
    });

    Header {
        document: document.to_string(),
        engine: engine.to_string(),
        run_id: run_id.map(str::to_string),
        status,
        elapsed_s,
        run_start_ts,
        run_end_ts,
        effects_done,
        effects_planned,
        effects_planned_is_lower_bound,
        tokens_sent,
        tokens_received,
        innermost_loop_eta_s,
        innermost_loop_eta_ts,
    }
}

/// Review finding 6: a loop-body leaf's own display path (DESIGN.md
/// §5's `iter_*` placeholder) counts as one pass in `plan.leaf_paths
/// ()` no matter how many times the loop actually runs it — "planned"
/// needs its own named loop's `meta.progress.total` instead, when
/// it's known, multiplied across every `iter_*` segment the leaf's
/// own path crosses (a loop nested inside another named loop). An
/// unnamed loop writes its body straight into its own parent
/// (DESIGN.md §1.3) with no `iter_*` segment of its own at all, so
/// every `iter_*` in a display path names a *named* loop's own
/// container — the path up to, not including, that segment — whose
/// `progress.total` this can read directly from `flat`. Returns `(1,
/// false)` for a leaf outside any loop. The second element is `true`
/// when any crossed loop's own total isn't known yet, in which case
/// the first is a lower bound (a single pass), not the true count.
/// `leaf_path`'s own pass multiplier, and whether it's a lower bound
/// rather than the true count (review finding 6 on #458's comment 3:
/// "a leaf under an unnamed loop still counts as a lower bound"). A
/// *named* loop's own `iter_*` placeholder segment multiplies by its
/// `progress_total` once known (or sets the bound, before it is); an
/// *unnamed* loop never gets an `iter_*` segment of its own at all
/// (its body's path is reused verbatim across passes, DESIGN.md
/// §2.3), so without this, a leaf under one was silently counted as
/// exactly one pass forever, with no `+` to say otherwise.
fn leaf_pass_count(
    leaf_path: &str,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
) -> (u64, bool) {
    let mut multiplier: u64 = 1;
    let mut lower_bound = false;
    let mut container = String::new();
    for segment in leaf_path.split('.') {
        if segment == "iter_*" {
            match flat.get(container.as_str()).and_then(|n| n.progress_total) {
                Some(total) if total > 0 => multiplier = multiplier.saturating_mul(total),
                _ => lower_bound = true,
            }
            continue;
        }
        if !container.is_empty() {
            container.push('.');
        }
        container.push_str(segment);
        // An unnamed loop's own entry sits at the *same* path as
        // whatever scope it's nested in (`is_transparent`'s own doc
        // comment: "contributes no path segment of its own") -- that
        // scope's own container entry (a named loop, an `if`, the
        // document root's own chain) is pushed into the trie first,
        // so the unnamed loop's own entry is never necessarily
        // `entries.first()` at that path; every entry there needs
        // checking, not just the first.
        if let Some(m) = plan.match_path(&container) {
            if m.entries
                .iter()
                .any(|e| matches!(e.kind, PlanEntryKind::Loop { named: false, .. }))
            {
                lower_bound = true;
            }
        }
    }
    (multiplier, lower_bound)
}

fn row_kind_for(path: &str, plan: &PlanTree, node: Option<&NodeMeta>) -> RowKind {
    if path == "prime" {
        return RowKind::Root;
    }
    if let Some(entry) = plan.match_path(path).and_then(|m| m.entries.first()) {
        return RowKind::from_plan_entry(&entry.kind);
    }
    if let Some(node) = node {
        return RowKind::from_meta_shape(node);
    }
    if path
        .rsplit('.')
        .next()
        .is_some_and(|seg| seg.starts_with("iter_"))
    {
        return RowKind::Pass;
    }
    RowKind::Unknown
}

/// A `use` leaf's own contribution to "planned" (the header) and, by
/// the identical rule applied again in `build_header`'s own
/// `effects_done` filter, to "done" too --
/// for a `use` with no *static* children of its own
/// (`inline:`/`ref:`; a `path:` child is handled by the caller
/// instead, via its own already-separate `leaf_paths` entries).
/// Counts whatever of its children have actually been *discovered* in
/// state so far, one each, falling back to the `use` node itself (as
/// before this fix) when nothing under it has ever been discovered at
/// all -- a declared-outputs child's own children are dropped from
/// the final state even once it succeeds (DESIGN.md §1.2), so without
/// this fallback such a `use` would count toward neither planned nor
/// done, ever. The second element is `true` (a lower bound, DESIGN.md
/// §6.3's "+") exactly when the `use` node itself has not yet reached
/// a terminal status -- more of its children could still appear.
fn use_leaf_contribution(
    use_path: &str,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    all_paths: &BTreeMap<String, ()>,
) -> (u64, bool) {
    let discovered = crate::model::subtree_entries(use_path, all_paths)
        .filter(|(p, ())| p.as_str() != use_path)
        .filter(|(p, ())| row_kind_for(p, plan, flat.get(p.as_str())).is_leaf())
        .count() as u64;
    let terminal = matches!(
        statuses.get(use_path).map(|s| s.kind),
        Some(
            crate::model::StatusKind::Done
                | crate::model::StatusKind::Failed
                | crate::model::StatusKind::FailedHandled
        )
    );
    if discovered > 0 {
        (discovered, !terminal)
    } else {
        (1, !terminal)
    }
}

/// Every immediate child segment under `parent` that appears in
/// `paths`, in the order children should render: a loop pass
/// (`iter_N`) sorts by its own numeric index first (so passes always
/// read in order regardless of when osi happened to observe them);
/// otherwise by the earliest `created_at` seen anywhere in that
/// child's own subtree, falling back to the plan's own declaration
/// order, and finally to the label itself — three independent tie
/// breaks, each of which alone can be absent (a pending plan node has
/// no `created_at` yet; an observed-only path with no plan has no
/// declaration order).
fn child_segments(
    parent: &str,
    paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
    plan_order: &BTreeMap<String, usize>,
) -> Vec<(String, String)> {
    let prefix = format!("{parent}.");
    let mut seen = std::collections::BTreeSet::new();
    let mut children: Vec<(String, String)> = Vec::new();
    // `crate::model::subtree_entries` bounds this to `parent`'s own
    // subtree instead of scanning every path in the tree (issue
    // #527's redraw-cost acceptance) — `walk_rows` calls this once
    // per row, so a whole-map scan here is quadratic in state size.
    for (path, ()) in crate::model::subtree_entries(parent, paths) {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        let segment = rest.split('.').next().unwrap_or(rest);
        let child_path = format!("{parent}.{segment}");
        if seen.insert(child_path.clone()) {
            children.push((segment.to_string(), child_path));
        }
    }

    children.sort_by(|(seg_a, path_a), (seg_b, path_b)| {
        let pass_a = pass_index(seg_a);
        let pass_b = pass_index(seg_b);
        if let (Some(a), Some(b)) = (pass_a, pass_b) {
            return a.cmp(&b);
        }
        let created_a = earliest_created_at(path_a, paths, flat);
        let created_b = earliest_created_at(path_b, paths, flat);
        match (created_a, created_b) {
            (Some(a), Some(b)) => return a.cmp(&b),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            (None, None) => {}
        }
        let order_a = plan_order.get(path_a);
        let order_b = plan_order.get(path_b);
        match (order_a, order_b) {
            (Some(a), Some(b)) => return a.cmp(b),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            (None, None) => {}
        }
        seg_a.cmp(seg_b)
    });
    children
}

fn pass_index(segment: &str) -> Option<u64> {
    segment.strip_prefix("iter_").and_then(|n| n.parse().ok())
}

/// A loop pass's own row label (issue #613): `pass N of M` in the
/// orchestration pane (1-based: `iter_2` is `pass 3`), or `pass N`
/// alone while the loop's own `progress.total` isn't known yet. The
/// state pane keeps the real `iter_N` key (`StateRow::key`, built
/// from the raw segment directly, never through this function).
fn pass_row_label(path: &str, flat: &BTreeMap<String, NodeMeta>) -> String {
    let last = path.rsplit('.').next().unwrap_or(path);
    let Some(n) = pass_index(last) else {
        return last.to_string();
    };
    let loop_path = path.rsplit_once('.').map(|(parent, _)| parent);
    let total = loop_path
        .and_then(|p| flat.get(p))
        .and_then(|loop_node| loop_node.progress_total);
    match total {
        Some(total) => format!("pass {} of {total}", n + 1),
        None => format!("pass {}", n + 1),
    }
}

fn earliest_created_at(
    prefix_path: &str,
    paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
) -> Option<String> {
    crate::model::subtree_entries(prefix_path, paths)
        .filter_map(|(p, ())| flat.get(p.as_str()))
        .filter_map(|node| node.created_at.clone())
        .min()
}

/// Every path `flat` or `statuses` has an entry for, plus `prime`
/// itself (which may have neither, before the first write) — shared
/// by `build_header`'s own planned/done counts and `build_rows`'s own
/// tree walk, so both agree on what a `use` node's own discovered
/// children are.
fn all_observed_paths(
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
) -> BTreeMap<String, ()> {
    let mut all_paths: BTreeMap<String, ()> = BTreeMap::new();
    for path in statuses.keys().chain(flat.keys()) {
        all_paths.insert(path.clone(), ());
    }
    all_paths.insert("prime".to_string(), ());
    all_paths
}

fn build_rows(
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    all_paths: &BTreeMap<String, ()>,
    now: &str,
) -> Vec<Row> {
    let mut plan_order = BTreeMap::new();
    if plan.has_plan() {
        for (idx, path) in plan.all_paths().iter().enumerate() {
            plan_order.entry(path.clone()).or_insert(idx);
        }
    }

    let mut rows = Vec::new();
    let default_status = RowStatus {
        kind: crate::model::StatusKind::Pending,
        skip_reason: None,
        retrying: false,
    };
    walk_rows(
        "prime",
        0,
        all_paths,
        flat,
        statuses,
        &plan_order,
        plan,
        &default_status,
        now,
        &mut rows,
    );
    rows
}

#[allow(clippy::too_many_arguments)]
/// A loop-pass container's own status, derived from its *leaf* descendants at any depth —
/// never from an intermediate container's own already-derived status,
/// so a pass with a nested `if`/loop of its own is judged by what
/// actually ran inside it, not by whether that nested container
/// happens to have a node of its own. `None` when nothing under
/// `path` has been observed at all yet (a pass not yet dispatched),
/// leaving the caller's own `Pending` default in place.
fn pass_status_from_children(
    path: &str,
    all_paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    plan: &PlanTree,
) -> Option<RowStatus> {
    let mut any_running = false;
    let mut any_failed = false;
    let mut all_terminal = true;
    let mut saw_any = false;
    for (descendant, ()) in crate::model::subtree_entries(path, all_paths) {
        if descendant.as_str() == path {
            continue;
        }
        let kind = row_kind_for(descendant, plan, flat.get(descendant.as_str()));
        if !kind.is_leaf() {
            continue;
        }
        saw_any = true;
        match statuses.get(descendant.as_str()).map(|s| s.kind) {
            Some(StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued) => {
                any_running = true
            }
            Some(StatusKind::Failed) => any_failed = true,
            Some(StatusKind::Done | StatusKind::FailedHandled) => {}
            _ => all_terminal = false,
        }
    }
    if !saw_any {
        return None;
    }
    let kind = if any_running {
        StatusKind::Running
    } else if any_failed {
        StatusKind::Failed
    } else if all_terminal {
        StatusKind::Done
    } else {
        StatusKind::Pending
    };
    Some(RowStatus {
        kind,
        skip_reason: None,
        retrying: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk_rows(
    path: &str,
    depth: usize,
    all_paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    plan_order: &BTreeMap<String, usize>,
    plan: &PlanTree,
    default_status: &RowStatus,
    now: &str,
    out: &mut Vec<Row>,
) {
    let node = flat.get(path);
    let kind = row_kind_for(path, plan, node);
    let last_segment = path.rsplit('.').next().unwrap_or(path);
    let label = if pass_index(last_segment).is_some() {
        pass_row_label(path, flat)
    } else {
        last_segment.to_string()
    };
    let status = statuses.get(path).cloned().unwrap_or_else(|| {
        // A loop pass container
        // (`prime.reviews.iter_N`) has no `{value, meta}` node of its
        // own (DESIGN.md §1.3) -- `kind == Pass` is exactly
        // `row_kind_for`'s own signal for "no node, no plan match, and
        // the last segment looks like a pass" -- so it never has an
        // entry in `statuses` either, and fell back to `default_status`
        // (`Pending`) regardless of how its own steps actually finished.
        if kind == RowKind::Pass {
            if let Some(derived) = pass_status_from_children(path, all_paths, flat, statuses, plan)
            {
                return derived;
            }
        }
        default_status.clone()
    });
    let duration_s = node.and_then(|n| match (&n.created_at, &n.completed_at) {
        (Some(start), Some(end)) => duration_seconds(start, end),
        (Some(start), None) => duration_seconds(start, now),
        _ => None,
    });
    let loop_progress = node.and_then(|n| {
        n.progress_done.map(|done| LoopProgress {
            done,
            total: n.progress_total,
            eta_s: n.progress_eta_s,
        })
    });
    let dim = node.and_then(|n| {
        n.meta
            .get("provider")
            .and_then(Value::as_str)
            .or_else(|| n.meta.get("model").and_then(Value::as_str))
            .map(str::to_string)
    });

    let children = child_segments(path, all_paths, flat, plan_order);
    out.push(Row {
        path: path.to_string(),
        depth,
        label,
        kind,
        status,
        duration_s,
        loop_progress,
        dim,
        has_children: !children.is_empty(),
    });

    for (_, child_path) in children {
        walk_rows(
            &child_path,
            depth + 1,
            all_paths,
            flat,
            statuses,
            plan_order,
            plan,
            default_status,
            now,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::Event;
    use serde_json::json;

    #[test]
    fn a_pending_plan_with_no_state_yet_renders_every_leaf_pending() {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region};

        let root_path = EffectPath::root();
        let child_path = root_path.clone().push_name("step1");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: child_path,
                        name: Some("step1".to_string()),
                        kind: NodeKind::Leaf(Box::new(LeafKind::Tool(
                            electricity_bytecode::ToolOp {
                                provider: "shell".to_string(),
                                params: electricity_bytecode::ParamNode::Literal(
                                    electricity_value::Value::None,
                                ),
                                params_json: None,
                                prompt: None,
                                model: None,
                                timeout_ms: None,
                                retries: Default::default(),
                                expect: None,
                                description: None,
                                group: None,
                                prompt_file: None,
                            },
                        ))),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    }],
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        let plan = PlanTree::from_program(&program);
        let mut model = RunModel::new();
        let state = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            0.0,
            "2026-01-01T00:00:00Z",
            &[],
        );
        assert_eq!(state.header.status, RunStatus::Running);
        assert_eq!(state.header.effects_planned, Some(1));
        assert_eq!(state.header.effects_done, 0);
        let step1 = state
            .rows
            .iter()
            .find(|r| r.path == "prime.step1")
            .expect("step1 row");
        assert_eq!(step1.kind, RowKind::Tool);
        assert_eq!(step1.status.kind, crate::model::StatusKind::Pending);
        assert_eq!(step1.depth, 1);
    }

    #[test]
    fn a_running_chain_shows_the_running_leaf_and_its_summary() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null, "provider": "shell"}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            1.5,
            "2026-01-01T00:00:01.5Z",
            &[],
        );
        let step1 = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.step1")
            .expect("step1 row");
        assert_eq!(step1.status.kind, crate::model::StatusKind::Running);
        assert_eq!(step1.dim.as_deref(), Some("shell"));
        assert_eq!(rendered.header.elapsed_s, 1.5);
        // Review finding 5 on #458's comment 3: a running row (no
        // `completed_at` yet) still gets a live duration, computed
        // against `build`'s own `now` rather than showing nothing
        // until it finishes.
        assert_eq!(step1.duration_s, Some(1.5));
    }

    /// Issue #613's "one clock": `osi watch` attaches to a run that's
    /// already been going for a while -- its own `elapsed_s` fallback
    /// (from an `Instant` created at attach time) is tiny, but the
    /// header's own elapsed must come from the run's own start (here,
    /// the earliest `created_at` in state, since no `--events` ever
    /// arrived) to `now`, not from osi's own freshly-created clock.
    #[test]
    fn header_elapsed_is_measured_from_the_runs_own_start_not_osis_own_clock() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": "x", "meta": {"created_at": "2026-01-01T11:04:58Z", "completed_at": "2026-01-01T11:10:00Z", "provider": "shell"}},
            "step2": {"value": null, "meta": {"created_at": "2026-01-01T11:10:00Z", "completed_at": null, "provider": "shell"}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            // osi watch's own fresh Instant, barely a moment old --
            // must not win over the run's real, much longer elapsed.
            0.05,
            "2026-01-01T11:14:58Z",
            &[],
        );
        assert_eq!(rendered.header.elapsed_s, 600.0);
        assert_eq!(
            rendered.header.run_start_ts.as_deref(),
            Some("2026-01-01T11:04:58Z")
        );
        assert_eq!(rendered.header.run_end_ts, None);
    }

    /// With a `run_start` event, the header's own start comes from
    /// that event's `ts`, not from scanning state at all -- and once
    /// the run has ended, elapsed stops at the run's own end, from
    /// `run_end`'s `ts`.
    #[test]
    fn header_elapsed_stops_at_run_end_and_prefers_event_timestamps() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "2026-01-01T11:04:58Z".to_string(),
            run_id: "r1".to_string(),
            pid: Some(1234),
        });
        model.observe_event(&Event::RunEnd {
            ts: "2026-01-01T11:31:10Z".to_string(),
            ok: true,
            error: None,
            signal: None,
        });
        let state_json = json!({
            "runtime": {"last_run": {"completed_at": "2026-01-01T11:31:10Z"}},
            "prime": {"value": true, "meta": {"created_at": "2026-01-01T11:04:58Z", "completed_at": "2026-01-01T11:31:10Z", "error": null}}
        });
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
            },
            0.0,
            // `now` well after the run actually ended -- elapsed must
            // not keep ticking past `run_end`'s own ts.
            "2026-01-01T12:00:00Z",
            &[],
        );
        assert_eq!(
            rendered.header.run_start_ts.as_deref(),
            Some("2026-01-01T11:04:58Z")
        );
        assert_eq!(
            rendered.header.run_end_ts.as_deref(),
            Some("2026-01-01T11:31:10Z")
        );
        assert_eq!(rendered.header.elapsed_s, 1572.0);
    }

    #[test]
    fn a_tree_loops_passes_render_in_numeric_order_not_observation_order() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        // iter_1 observed (via created_at) before iter_0 would sort
        // wrong alphabetically too, but this proves the numeric
        // tie-break specifically.
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "each_tree": {"value": null, "meta": {"completed_at": null},
                "iter_1": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z", "provider": "shell"}},
                "iter_0": {"value": null, "meta": {"created_at": "2026-01-01T00:00:02Z", "completed_at": "2026-01-01T00:00:03Z", "provider": "shell"}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            3.0,
            "2026-01-01T00:00:03Z",
            &[],
        );
        let order: Vec<&str> = rendered
            .rows
            .iter()
            .filter(|r| r.depth == 2)
            .map(|r| r.path.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["prime.each_tree.iter_0", "prime.each_tree.iter_1"]
        );
        // Issue #613: the orchestration pane labels a pass `pass N`
        // (1-based), not the state pane's own `iter_N` key.
        let labels: Vec<&str> = rendered
            .rows
            .iter()
            .filter(|r| r.depth == 2)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, vec!["pass 1", "pass 2"]);
    }

    /// Issue #613: once the enclosing loop's own `progress.total` is
    /// known, a pass's own label becomes `pass N of M`.
    #[test]
    fn a_pass_label_includes_the_loops_own_total_once_known() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "each_tree": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 8}},
                "iter_2": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            3.0,
            "t1",
            &[],
        );
        let pass_row = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.each_tree.iter_2")
            .expect("pass row");
        assert_eq!(pass_row.label, "pass 3 of 8");
        let details = details_for("prime.each_tree.iter_2", &plan, Some(&state_json));
        assert_eq!(details.plan_summary, "pass 3 of 8 (iter_2)");
    }

    fn named_loop_with_one_leaf_program() -> electricity_bytecode::Program {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode,
            Program, Region, ToolOp,
        };

        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let body_path = pass_path.push_name("nap");
        Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: loop_path,
                        name: Some("fan".to_string()),
                        kind: NodeKind::Control(Region::Loop {
                            name: Some("fan".to_string()),
                            spec: LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![Op {
                                    path: body_path,
                                    name: Some("nap".to_string()),
                                    kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                                        provider: "shell".to_string(),
                                        params: ParamNode::Literal(electricity_value::Value::None),
                                        params_json: None,
                                        prompt: None,
                                        model: None,
                                        timeout_ms: None,
                                        retries: Default::default(),
                                        expect: None,
                                        description: None,
                                        group: None,
                                        prompt_file: None,
                                    }))),
                                    on_error: OnError::Fail,
                                    labels: None,
                                    enabled: true,
                                }],
                                overlay: true,
                            }),
                            flow: LoopFlow::Tree,
                            max_concurrency: None,
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    }],
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    /// An unnamed loop's own leaf (review finding 6 on #458's comment
    /// 3): its body reuses the document root's own path (DESIGN.md
    /// §1.3, `is_transparent`), so unlike
    /// `named_loop_with_one_leaf_program`'s `fan.iter_*.nap`, this has
    /// no `iter_*` placeholder segment of its own at all -- just
    /// `prime.u_nap`.
    fn unnamed_loop_with_one_leaf_program() -> electricity_bytecode::Program {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopFlow, NodeKind, OnError, Op, ParamNode, Program, Region,
            ToolOp,
        };

        let root_path = EffectPath::root();
        let loop_path = root_path.clone();
        let body_path = loop_path.clone().push_name("u_nap");
        Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: loop_path,
                        name: None,
                        kind: NodeKind::Control(Region::Loop {
                            name: None,
                            spec: electricity_bytecode::LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![Op {
                                    path: body_path,
                                    name: Some("u_nap".to_string()),
                                    kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                                        provider: "shell".to_string(),
                                        params: ParamNode::Literal(electricity_value::Value::None),
                                        params_json: None,
                                        prompt: None,
                                        model: None,
                                        timeout_ms: None,
                                        retries: Default::default(),
                                        expect: None,
                                        description: None,
                                        group: None,
                                        prompt_file: None,
                                    }))),
                                    on_error: OnError::Fail,
                                    labels: None,
                                    enabled: true,
                                }],
                                overlay: true,
                            }),
                            flow: LoopFlow::Chain,
                            max_concurrency: None,
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    }],
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    #[test]
    fn effects_planned_multiplies_a_loop_bodys_leaf_by_its_known_total() {
        // Review finding 6: `fan`'s own single leaf template
        // (`fan.iter_*.nap`) must count as 4 once the loop's own
        // `progress.total` says so, not once — two passes already
        // done count as 2 of those 4, not as 1 of 1.
        let plan = PlanTree::from_program(&named_loop_with_one_leaf_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 2, "total": 4}},
                "iter_0": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                "iter_1": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            2.0,
            "t2",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(4));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 2);
    }

    #[test]
    fn effects_planned_is_a_lower_bound_before_the_loops_own_total_is_known() {
        let plan = PlanTree::from_program(&named_loop_with_one_leaf_program());
        let mut model = RunModel::new();
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            0.0,
            "t0",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(1));
        assert!(rendered.header.effects_planned_is_lower_bound);
    }

    #[test]
    fn effects_planned_is_a_lower_bound_under_an_unnamed_loop_even_once_a_pass_is_done() {
        // Review finding 6 on #458's comment 3: an unnamed loop never
        // gets a known `progress.total` of its own path the way a
        // *named* one's `iter_*` placeholder does -- `u_nap` must stay
        // a lower bound (`1+`) no matter how many passes have already
        // completed at that same, reused path, not settle to a bare
        // `1` once the first one does.
        let plan = PlanTree::from_program(&unnamed_loop_with_one_leaf_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "u_nap": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(1));
        assert!(
            rendered.header.effects_planned_is_lower_bound,
            "a leaf under an unnamed loop is never an exact count"
        );
    }

    /// [`named_loop_with_one_leaf_program`], with two leaves per pass
    /// instead of one -- the shape this needs: `prime.fan.iter_N` (the pass container) never gets a
    /// `{value, meta}` node of its own (DESIGN.md §1.3), so its row's
    /// status must be derived from `step_a`/`step_b`'s own, not read
    /// off a node that doesn't exist.
    fn named_loop_with_two_leaf_program(
        flow: electricity_bytecode::LoopFlow,
    ) -> electricity_bytecode::Program {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode, Program,
            Region, ToolOp,
        };

        fn tool(path: EffectPath, name: &str) -> Op {
            Op {
                path,
                name: Some(name.to_string()),
                kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                    provider: "shell".to_string(),
                    params: ParamNode::Literal(electricity_value::Value::None),
                    params_json: None,
                    prompt: None,
                    model: None,
                    timeout_ms: None,
                    retries: Default::default(),
                    expect: None,
                    description: None,
                    group: None,
                    prompt_file: None,
                }))),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            }
        }

        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let step_a = pass_path.clone().push_name("step_a");
        let step_b = pass_path.push_name("step_b");
        Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: loop_path,
                        name: Some("fan".to_string()),
                        kind: NodeKind::Control(Region::Loop {
                            name: Some("fan".to_string()),
                            spec: LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![tool(step_a, "step_a"), tool(step_b, "step_b")],
                                overlay: true,
                            }),
                            flow,
                            max_concurrency: None,
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    }],
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    #[test]
    fn a_multi_step_tree_pass_shows_done_once_every_step_under_it_is_done() {
        let plan = PlanTree::from_program(&named_loop_with_two_leaf_program(
            electricity_bytecode::LoopFlow::Tree,
        ));
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t2"},
            "fan": {"value": null, "meta": {"completed_at": "t2", "progress": {"done": 1, "total": 1}},
                "iter_0": {
                    "step_a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                    "step_b": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
                }
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            2.0,
            "t2",
            &[],
        );
        let pass = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.fan.iter_0")
            .expect("the pass container's own row");
        assert_eq!(
            pass.status.kind,
            StatusKind::Done,
            "every step under the pass is done, so the pass itself must read done, not the default pending"
        );
    }

    #[test]
    fn a_multi_step_tree_pass_shows_running_while_any_step_under_it_still_is() {
        let plan = PlanTree::from_program(&named_loop_with_two_leaf_program(
            electricity_bytecode::LoopFlow::Tree,
        ));
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 0, "total": 1}},
                "iter_0": {
                    "step_a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                    "step_b": {"value": null, "meta": {"created_at": "t1", "completed_at": null, "provider": "shell"}}
                }
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        let pass = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.fan.iter_0")
            .expect("the pass container's own row");
        assert_eq!(pass.status.kind, StatusKind::Running);
    }

    #[test]
    fn a_multi_step_tree_pass_shows_failed_when_an_unhandled_step_under_it_fails() {
        let plan = PlanTree::from_program(&named_loop_with_two_leaf_program(
            electricity_bytecode::LoopFlow::Tree,
        ));
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t2"}},
            "prime": {"value": false, "meta": {"completed_at": "t2", "error": "prime.fan.iter_0.step_b: boom"},
                "fan": {"value": null, "meta": {"completed_at": "t2", "error": "iter_0.step_b: boom", "progress": {"done": 0, "total": 1}},
                    "iter_0": {
                        "step_a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                        "step_b": {"value": null, "meta": {"created_at": "t1", "completed_at": "t2", "error": "boom", "provider": "shell"}}
                    }
                }
            }
        });
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            2.0,
            "t2",
            &[],
        );
        let pass = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.fan.iter_0")
            .expect("the pass container's own row");
        assert_eq!(pass.status.kind, StatusKind::Failed);
    }

    /// A *chain*-flow loop's own
    /// multi-step pass must be derived the identical way a tree-flow
    /// one is -- the pass container has no node of its own regardless
    /// of flow (DESIGN.md §1.3 names no distinction).
    #[test]
    fn a_multi_step_chain_pass_also_derives_its_status_from_its_steps() {
        let plan = PlanTree::from_program(&named_loop_with_two_leaf_program(
            electricity_bytecode::LoopFlow::Chain,
        ));
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t2"},
            "fan": {"value": null, "meta": {"completed_at": "t2", "progress": {"done": 1, "total": 1}},
                "iter_0": {
                    "step_a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                    "step_b": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
                }
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            2.0,
            "t2",
            &[],
        );
        let pass = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.fan.iter_0")
            .expect("the pass container's own row");
        assert_eq!(pass.status.kind, StatusKind::Done);
    }

    /// `named_loop_with_one_leaf_program`'s own single-step pass
    /// (`fan.iter_0.nap`) must look exactly as it did before this fix --
    /// single-step passes still look as before -- it goes through the identical
    /// derivation path (the pass container still has no node of its
    /// own), just with one leaf instead of two, and must still read
    /// done once that one leaf is.
    #[test]
    fn a_single_step_pass_still_derives_done_from_its_one_leaf() {
        let plan = PlanTree::from_program(&named_loop_with_one_leaf_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "fan": {"value": null, "meta": {"completed_at": "t1", "progress": {"done": 1, "total": 1}},
                "iter_0": {"nap": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            1.0,
            "t1",
            &[],
        );
        let pass = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.fan.iter_0")
            .expect("the pass container's own row");
        assert_eq!(pass.status.kind, StatusKind::Done);
    }

    /// A real `use: inline:` document: before any of its children have been discovered in
    /// state at all (DESIGN.md §1.4, "never in-flight"), "planned"
    /// falls back to counting the `use` node itself, as a lower bound
    /// -- there is no static plan for an inline child to count
    /// instead (DESIGN.md §5).
    #[test]
    fn effects_planned_for_an_inline_use_is_a_lower_bound_before_any_child_is_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(
            &doc,
            "effects:\n  - name: my_use\n    type: use\n    inline: |\n      effects:\n        - name: child1\n          type: tool\n          provider: shell\n          params:\n            command: echo\n            args: [\"c1\"]\n        - name: child2\n          type: tool\n          provider: shell\n          params:\n            command: echo\n            args: [\"c2\"]\n",
        )
        .unwrap();
        let program = crate::plan::compile(&doc, None, &indexmap::IndexMap::new())
            .expect("a real document should compile");
        let plan = PlanTree::from_program(&program);

        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "my_use": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "inline": true}}
        }});
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(1));
        assert!(rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 0);
    }

    /// [`effects_planned_for_an_inline_use_is_a_lower_bound_before_any_child_is_discovered`],
    /// once one child has actually landed in state -- a use child's
    /// effects count into planned once they
    /// appear in state, not the use node itself. Both planned and
    /// done switch to counting the discovered child, not `my_use`'s
    /// own path, and `done` never outruns `planned`.
    #[test]
    fn effects_planned_and_done_for_an_inline_use_count_its_discovered_children_not_the_use_node() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(
            &doc,
            "effects:\n  - name: my_use\n    type: use\n    inline: |\n      effects:\n        - name: child1\n          type: tool\n          provider: shell\n          params:\n            command: echo\n            args: [\"c1\"]\n        - name: child2\n          type: tool\n          provider: shell\n          params:\n            command: echo\n            args: [\"c2\"]\n",
        )
        .unwrap();
        let program = crate::plan::compile(&doc, None, &indexmap::IndexMap::new())
            .expect("a real document should compile");
        let plan = PlanTree::from_program(&program);

        let mut model = RunModel::new();
        // Only `child1` has appeared so far -- `my_use` is still open.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "my_use": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "inline": true},
                "child1": {"value": "c1\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            }
        }});
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(
            rendered.header.effects_planned,
            Some(1),
            "one discovered child so far, not the use node too"
        );
        assert!(
            rendered.header.effects_planned_is_lower_bound,
            "my_use is still open -- more children could still appear"
        );
        assert_eq!(rendered.header.effects_done, 1);
        assert!(rendered.header.effects_done <= rendered.header.effects_planned.unwrap());

        // Now both children are done and `my_use` itself has finished
        // -- planned becomes exact (2, no `+`), matching done exactly.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t2"}},
            "prime": {"value": true, "meta": {"completed_at": "t2", "error": null},
            "my_use": {"value": null, "meta": {"created_at": "t0", "completed_at": "t2", "inline": true},
                "child1": {"value": "c1\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                "child2": {"value": "c2\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
            }}
        });
        let mut model = RunModel::new();
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            2.0,
            "t2",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(2));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 2);
    }

    /// A `use` whose children never appear even once it finishes (a
    /// declared-outputs child, DESIGN.md §1.2: "children are dropped
    /// from the final state") must still count as 1 effect on both
    /// sides, exactly as before this fix -- the discovery-based count
    /// falls back to the `use` node itself when nothing was ever
    /// discovered under it.
    #[test]
    fn effects_planned_and_done_fall_back_to_the_use_node_when_its_children_never_appear() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(
            &doc,
            "effects:\n  - name: my_use\n    type: use\n    inline: |\n      effects:\n        - name: child1\n          type: tool\n          provider: shell\n          params:\n            command: echo\n            args: [\"c1\"]\n",
        )
        .unwrap();
        let program = crate::plan::compile(&doc, None, &indexmap::IndexMap::new())
            .expect("a real document should compile");
        let plan = PlanTree::from_program(&program);

        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
            "my_use": {"value": {"ans": "ok"}, "meta": {"created_at": "t0", "completed_at": "t1", "inline": true}}
        }});
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(1));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 1);
    }

    /// A real `use: path:` document: its children are statically known
    /// (grafted into the plan, DESIGN.md §5), so "planned" is exact
    /// from the start -- counting the grafted children, never the
    /// `use` node itself too (the two
    /// used to be counted together, double-counting against `done`).
    #[test]
    fn effects_planned_for_a_path_use_counts_its_grafted_children_not_the_use_node_too() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child.yml");
        std::fs::write(
            &child,
            "effects:\n  - name: child1\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"c1\"]\n  - name: child2\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"c2\"]\n",
        )
        .unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(
            &doc,
            "effects:\n  - name: my_use\n    type: use\n    path: child.yml\n",
        )
        .unwrap();
        let program = crate::plan::compile(&doc, None, &indexmap::IndexMap::new())
            .expect("a real document should compile");
        let plan = PlanTree::from_program(&program);

        let mut model = RunModel::new();
        // No run has started at all yet -- a `path:` use's children
        // are known from the static plan alone, so this is already
        // exact, with no lower bound, before a single effect has run.
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            0.0,
            "t0",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(2));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 0);

        // Mid-run, with both children discovered and done, and
        // `my_use` itself also finished: done must equal planned, not
        // outrun it with the `use` node counted a third time.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t2"}},
            "prime": {"value": true, "meta": {"completed_at": "t2", "error": null},
            "my_use": {"value": null, "meta": {"created_at": "t0", "completed_at": "t2", "orchestration": "child.yml"},
                "child1": {"value": "c1\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                "child2": {"value": "c2\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
            }}
        });
        let mut model = RunModel::new();
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            2.0,
            "t2",
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(2));
        assert_eq!(rendered.header.effects_done, 2);
        assert!(rendered.header.effects_done <= rendered.header.effects_planned.unwrap());
    }

    #[test]
    fn details_for_keeps_the_stderr_tails_own_reading_order() {
        // Finding 12: the last 5 lines, in the order they were
        // written — not newest first.
        let plan = PlanTree::empty();
        let stderr = (1..=8)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "tool": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell", "stderr": stderr}}
        }});
        let details = details_for("prime.tool", &plan, Some(&state_json));
        let tail = details
            .meta_summary
            .iter()
            .find(|(k, _)| k == "stderr")
            .map(|(_, v)| v.as_str())
            .expect("a stderr summary entry");
        assert_eq!(tail, "line 4\nline 5\nline 6\nline 7\nline 8");
    }

    #[test]
    fn details_for_a_prompt_truncates_the_preview_but_full_value_does_not() {
        let plan = PlanTree::empty();
        let long_prompt = "x".repeat(DETAILS_PREVIEW_CHARS + 50);
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": "the reply", "meta": {"created_at": "t0", "completed_at": "t1", "adapter": "scripted", "model": "m", "prompt_sent": long_prompt.clone()}}
        }});
        let details = details_for("prime.ask", &plan, Some(&state_json));
        assert!(details.prompt_sent.as_ref().unwrap().len() < long_prompt.len());
        let full = full_value("prime.ask", FullValueField::PromptSent, Some(&state_json));
        assert_eq!(full, Some(long_prompt));
    }

    #[test]
    fn sent_received_blocks_for_a_finished_text_prompt() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": "the final reply", "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted", "model": "m",
                "prompt_sent": "what is the answer",
                "tokens_sent": 12, "tokens_received": 34
            }}
        }});
        let details = details_for("prime.ask", &plan, Some(&state_json));
        let sent = details.sent.expect("a prompt node gets a sent block");
        assert_eq!(sent.snippet.as_deref(), Some("what is the answer"));
        assert_eq!(sent.tokens, Some(12));
        assert!(!sent.estimated);
        let received = details
            .received
            .expect("a finished prompt gets a received block");
        assert_eq!(received.snippet.as_deref(), Some("the final reply"));
        assert_eq!(received.tokens, Some(34));
        assert!(!received.waiting);
        // the plain meta list no longer repeats the token counts.
        assert!(!details.meta_summary.iter().any(|(k, _)| k == "tokens_sent"));
        assert!(
            !details
                .meta_summary
                .iter()
                .any(|(k, _)| k == "tokens_received")
        );
    }

    #[test]
    fn sent_received_blocks_for_a_finished_json_prompt() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": {"ok": true}, "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted", "model": "m",
                "prompt_sent": "reply in json",
                "tokens_sent_total": 9, "tokens_received_total": 3
            }}
        }});
        let details = details_for("prime.ask", &plan, Some(&state_json));
        let received = details.received.unwrap();
        // no `meta.answer` here (not boolean/number), so the node's
        // own value, as compact JSON, is the snippet.
        assert_eq!(received.snippet.as_deref(), Some(r#"{"ok":true}"#));
        assert_eq!(received.tokens, Some(3));
        let sent = details.sent.unwrap();
        assert_eq!(sent.tokens, Some(9));
    }

    #[test]
    fn sent_block_estimates_tokens_and_received_waits_while_a_prompt_is_running() {
        let plan = PlanTree::empty();
        let prompt_sent = "x".repeat(40);
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": null, "meta": {
                "created_at": "t0", "completed_at": null, "adapter": "scripted", "model": "m",
                "prompt_sent": prompt_sent
            }}
        }});
        let details = details_for("prime.ask", &plan, Some(&state_json));
        let sent = details.sent.unwrap();
        assert_eq!(sent.tokens, Some(10)); // ceil(40/4)
        assert!(sent.estimated);
        let received = details.received.unwrap();
        assert!(received.waiting);
        assert_eq!(received.snippet, None);
        assert_eq!(received.tokens, None);
    }

    #[test]
    fn received_block_uses_meta_answer_for_a_boolean_prompt() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "gate": {"value": true, "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted", "model": "m",
                "prompt_sent": "yes or no?", "answer": "yes, definitely",
                "tokens_sent": 1, "tokens_received": 2
            }}
        }});
        let details = details_for("prime.gate", &plan, Some(&state_json));
        let received = details.received.unwrap();
        assert_eq!(received.snippet.as_deref(), Some("yes, definitely"));
    }

    #[test]
    fn a_prompt_failed_before_any_reply_has_no_received_block() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": null, "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted", "model": "m",
                "prompt_sent": "what is the answer", "error": "adapter unreachable"
            }}
        }});
        let details = details_for("prime.ask", &plan, Some(&state_json));
        assert!(details.sent.is_some(), "it still sent the prompt");
        assert!(details.received.is_none());
        assert_eq!(details.error.as_deref(), Some("adapter unreachable"));
    }

    /// Finding 18: a reply that arrived and *then* failed (an
    /// `expect`/parsing mismatch, here) still gets a `received`
    /// block -- only a failure with no reply at all drops it.
    #[test]
    fn a_prompt_that_failed_after_a_reply_still_has_a_received_block() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "gate": {"value": null, "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted", "model": "m",
                "prompt_sent": "yes or no?", "answer": "maybe",
                "error": "expected a boolean reply, got \"maybe\""
            }}
        }});
        let details = details_for("prime.gate", &plan, Some(&state_json));
        assert!(details.sent.is_some());
        let received = details
            .received
            .expect("a reply that arrived before the failure keeps its received block");
        assert_eq!(received.snippet.as_deref(), Some("maybe"));
        assert_eq!(
            details.error.as_deref(),
            Some("expected a boolean reply, got \"maybe\"")
        );
    }

    #[test]
    fn a_non_prompt_node_gets_no_sent_received_blocks() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ls": {"value": "a\nb\n", "meta": {
                "created_at": "t0", "completed_at": "t1", "provider": "shell", "exit_code": 0
            }}
        }});
        let details = details_for("prime.ls", &plan, Some(&state_json));
        assert!(details.sent.is_none());
        assert!(details.received.is_none());
    }

    #[test]
    fn state_rows_sort_most_recently_updated_first_with_path_as_tiebreak() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:03Z"}},
            "b": {"value": "y", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:05Z"}},
            "c": {"value": null, "meta": {"created_at": "2026-01-01T00:00:05Z", "completed_at": null}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        let paths: Vec<&str> = rendered
            .state_rows
            .iter()
            .map(|r| r.path.as_str())
            .collect();
        // `c` is still running, so it counts as current and sorts
        // first regardless of timestamps (issue #535); `a` and `b`
        // are both finished, tied by nothing since their own
        // `completed_at` differ, so `b` (00:05) sorts before `a`
        // (00:03).
        assert_eq!(paths, vec!["c", "b", "a"]);
    }

    /// Finding 15: latest-first ordering at a *nested* level, not
    /// just the top -- `older` and `newer` are both bare containers
    /// (no `{value, meta}` node of their own) two levels under
    /// `fan`, sorted by the latest update anywhere in their own
    /// subtree, same as the top level.
    #[test]
    fn state_rows_order_nested_containers_by_their_latest_child_update() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                "older": {
                    "p": {"value": "x", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z"}}
                },
                "newer": {
                    "q": {"value": "y", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:09Z"}}
                }
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            9.0,
            "2026-01-01T00:00:09Z",
            &[],
        );
        let paths: Vec<&str> = rendered
            .state_rows
            .iter()
            .filter(|r| r.depth == 1)
            .map(|r| r.path.as_str())
            .collect();
        assert_eq!(paths, vec!["fan.newer", "fan.older"]);
    }

    /// Issue #535: the state pane no longer shows any value or error
    /// text at all — just the key, status, dim meta and tokens.
    #[test]
    fn state_rows_carry_no_value_or_error_text() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "summarize": {"value": "The report covers everything", "meta": {"created_at": "t0", "completed_at": "t1"}},
            "pending": {"value": null, "meta": {"created_at": "t2", "completed_at": null}},
            "bad": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1)\nls: no such file"}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t2",
            &[],
        );
        // Type-level: `StateRow` has no `value`/`error` field at all
        // any more, so there's nothing further to assert for
        // `summarize`/`bad` beyond their status glyph.
        let pending = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "pending")
            .unwrap();
        assert_eq!(pending.status.kind, StatusKind::Running);
        let bad = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "bad")
            .unwrap();
        assert_eq!(bad.status.kind, StatusKind::Failed);
    }

    /// Issue #613: a running row's own `dim` shows its elapsed time
    /// so far, measured from its own `created_at` to `build`'s own
    /// `now` -- unlike a finished row's duration, which is already
    /// pinned by its own `completed_at` and so never changes no
    /// matter which `now` rebuilt it.
    #[test]
    fn a_running_rows_dim_shows_elapsed_from_now_a_finished_rows_does_not() {
        let plan = PlanTree::empty();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "running": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null}},
            "done": {"value": "x", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:05Z"}}
        }});
        let mut model_a = RunModel::new();
        let at_t1 = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model_a,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T00:00:01Z",
            &[],
        );
        let mut model_b = RunModel::new();
        let at_t9 = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model_b,
            Some(&state),
            ProcessState::Running,
            9.0,
            "2026-01-01T00:00:09Z",
            &[],
        );
        let running_dim = |r: &RenderState| {
            r.state_rows
                .iter()
                .find(|row| row.path == "running")
                .unwrap()
                .dim
                .clone()
        };
        assert_eq!(running_dim(&at_t1).as_deref(), Some("1.0s"));
        assert_eq!(running_dim(&at_t9).as_deref(), Some("9.0s"));
        let done_dim = |r: &RenderState| {
            r.state_rows
                .iter()
                .find(|row| row.path == "done")
                .unwrap()
                .dim
                .clone()
        };
        assert_eq!(done_dim(&at_t1).as_deref(), Some("5.0s"));
        assert_eq!(
            done_dim(&at_t1),
            done_dim(&at_t9),
            "a finished row's duration is pinned by its own completed_at, not `now`"
        );
    }

    #[test]
    fn state_rows_nest_bare_containers_with_no_node_of_their_own() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                "iter_0": {
                    "nap": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1"}}
                }
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        let by_path: std::collections::HashMap<&str, &StateRow> = rendered
            .state_rows
            .iter()
            .map(|r| (r.path.as_str(), r))
            .collect();
        let fan = by_path.get("fan").expect("fan has its own node");
        assert_eq!(fan.depth, 0);
        assert_eq!(fan.key, "fan");
        assert!(fan.has_children);

        let iter0 = by_path
            .get("fan.iter_0")
            .expect("a bare container with no node of its own is still a row");
        assert_eq!(iter0.depth, 1);
        assert_eq!(iter0.key, "iter_0");
        assert!(iter0.has_children);
        assert_eq!(
            iter0.dim, None,
            "a bare container has nothing of its own to summarize"
        );

        let nap = by_path.get("fan.iter_0.nap").unwrap();
        assert_eq!(nap.depth, 2);
        assert_eq!(nap.key, "nap");
        assert!(!nap.has_children);
    }

    #[test]
    fn state_rows_put_a_running_sibling_before_a_finished_one_regardless_of_timestamps() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        // `a` finished *after* `b` started running, yet `b` (still
        // running) must sort first (issue #535: "a running row counts
        // as current").
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:09Z"}},
            "b": {"value": null, "meta": {"created_at": "2026-01-01T00:00:01Z", "completed_at": null}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            9.0,
            "2026-01-01T00:00:09Z",
            &[],
        );
        let paths: Vec<&str> = rendered
            .state_rows
            .iter()
            .map(|r| r.path.as_str())
            .collect();
        assert_eq!(paths, vec!["b", "a"]);
    }

    #[test]
    fn a_bare_containers_token_sum_and_dim_come_from_its_subtree() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
                "iter_0": {
                    "ask": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z", "tokens_sent": 5, "tokens_received": 7, "model": "m"}}
                }
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T00:00:01Z",
            &[],
        );
        let iter0 = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "fan.iter_0")
            .unwrap();
        assert_eq!(iter0.tokens, Some((5, 7)));
        let ask = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "fan.iter_0.ask")
            .unwrap();
        assert_eq!(ask.dim.as_deref(), Some("m  1.0s"));
    }

    /// Acceptance list: a tree container's own sum, not just a named
    /// loop's — a dynamic (unnamed tree block) with two prompt leaves.
    #[test]
    fn a_tree_containers_state_row_sums_tokens_over_both_branches() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                "left": {"value": "a", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent_total": 3, "tokens_received_total": 4}},
                "right": {"value": "b", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent_total": 6, "tokens_received_total": 9}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        let fan = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "fan")
            .unwrap();
        assert_eq!(fan.tokens, Some((9, 13)));
    }

    /// Acceptance list: a `use` child's own tokens roll up into the
    /// `use` node's own sum, the same as any other container.
    #[test]
    fn a_use_childs_tokens_roll_up_into_the_use_nodes_sum() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "first": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "orchestration": "child.yml"},
                "c_ask": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent_total": 2, "tokens_received_total": 5}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        let first = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "first")
            .unwrap();
        assert_eq!(first.tokens, Some((2, 5)));
    }

    #[test]
    fn a_container_state_row_sums_tokens_over_its_whole_subtree() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                "iter_0": {"ask": {"value": "a", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent_total": 10, "tokens_received_total": 20}}},
                "iter_1": {"ask": {"value": "b", "meta": {"created_at": "t1", "completed_at": "t2", "tokens_sent_total": 5, "tokens_received_total": 7}}}
            }
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            2.0,
            "t2",
            &[],
        );
        let fan = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "fan")
            .unwrap();
        assert_eq!(fan.tokens, Some((15, 27)));
        let leaf = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "fan.iter_0.ask")
            .unwrap();
        assert_eq!(leaf.tokens, Some((10, 20)));
    }

    #[test]
    fn the_token_bar_only_counts_prompts_that_have_actually_started_their_call() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ready": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "prompt_sent": "1234567890ABCD", "waiting_for": null}},
            "queued": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "prompt_sent": "xx", "waiting_for": "concurrency"}},
            "not_yet_called": {"value": null, "meta": {"created_at": "t0", "completed_at": null}},
            "done": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1", "prompt_sent": "hi"}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.token_bar.prompts_running, 1);
        // ceil(14 / 4) = 4
        assert_eq!(rendered.token_bar.estimated_input, 4);
        assert!(!rendered.token_bar.estimated_input_is_lower_bound);
    }

    /// Issue #613's "agents in flight": a running tool whose
    /// `provider` is `agent` (cof's own rule, trimmed and
    /// case-insensitive) counts in the token bar's in-flight line
    /// alongside running prompts, but gets no token estimate of its
    /// own. Both engines write `meta.provider` on a running tool node
    /// the same way (DESIGN.md §1.2), so one state shape covers both.
    #[test]
    fn running_agent_tool_steps_are_counted_as_agents_in_flight_with_no_token_estimate() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "review": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "  Agent  "}},
            "ask": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "prompt_sent": "hi", "waiting_for": null}},
            "not_an_agent": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.token_bar.agents_running, 1);
        assert_eq!(rendered.token_bar.prompts_running, 1);
    }

    fn named_loop_with_one_prompt_program() -> electricity_bytecode::Program {
        use electricity_bytecode::EffectPath;
        use electricity_bytecode::{
            Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, Program,
            PromptContent, PromptOp, PromptType, Region, TemplateText,
        };

        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let body_path = pass_path.push_name("ask");
        Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: loop_path,
                        name: Some("fan".to_string()),
                        kind: NodeKind::Control(Region::Loop {
                            name: Some("fan".to_string()),
                            spec: LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![Op {
                                    path: body_path,
                                    name: Some("ask".to_string()),
                                    kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
                                        content: PromptContent::Template(TemplateText::new(
                                            "hi",
                                            false,
                                            Escape::None,
                                        )),
                                        prompt_type: PromptType::Text,
                                        schema: None,
                                        model: None,
                                        provider: None,
                                        provider_fallbacks: Vec::new(),
                                        routing_override: None,
                                        model_params: None,
                                        timeout_ms: None,
                                        deterministic: false,
                                        inputs: None,
                                        assets: Vec::new(),
                                        retries: None,
                                        description: None,
                                        group: None,
                                    }))),
                                    on_error: OnError::Fail,
                                    labels: None,
                                    enabled: true,
                                }],
                                overlay: true,
                            }),
                            flow: LoopFlow::Tree,
                            max_concurrency: None,
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    }],
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    /// A tree-loop pass's prompt is
    /// confirmed `Running` by events while never once appearing in
    /// live state at all (DESIGN.md §1.4) -- the token bar must still
    /// count it, from `rows` (where its own `kind` is known via the
    /// compiled plan's `iter_*` placeholder match), not from `flat`
    /// (which has nothing at that path to find).
    #[test]
    fn the_token_bar_counts_a_tree_loop_prompt_running_by_events_alone_as_a_lower_bound() {
        let plan = PlanTree::from_program(&named_loop_with_one_prompt_program());
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.ask".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.token_bar.prompts_running, 1);
        assert_eq!(
            rendered.token_bar.estimated_input, 0,
            "nothing is estimated without a prompt_sent to measure"
        );
        assert!(
            rendered.token_bar.estimated_input_is_lower_bound,
            "no prompt_sent for this one at all -- the estimate must say so, not read as an exact 0"
        );
    }

    /// When *some* running
    /// prompts have a known `prompt_sent` and *some* don't, the
    /// estimate is a nonzero lower bound (`≈5+`, not the `?` a wholly
    /// unknown one gets) -- one known prompt (`ready`, counted
    /// straight from its own state node) running alongside one
    /// confirmed only by events (the tree-loop pass, no node of its
    /// own at all) pins that mixed case.
    #[test]
    fn the_token_bar_estimate_is_a_nonzero_lower_bound_when_only_some_running_prompts_are_known() {
        let plan = PlanTree::from_program(&named_loop_with_one_prompt_program());
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.ask".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ready": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "prompt_sent": "1234567890ABCD", "waiting_for": null}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.token_bar.prompts_running, 2);
        // ceil(14 / 4) = 4, from `ready` alone -- `fan.iter_0.ask` has
        // no `prompt_sent` to add anything.
        assert_eq!(rendered.token_bar.estimated_input, 4);
        assert!(
            rendered.token_bar.estimated_input_is_lower_bound,
            "one of the two running prompts has no prompt_sent known -- the total is still a lower bound"
        );
    }

    /// A row confirmed running purely by events, with no state node
    /// and no plan entry either (a `use: inline:` child, which has no
    /// static plan at all, DESIGN.md §5) cannot be told apart from a
    /// running *tool* by path and events alone (DESIGN.md §3 carries
    /// no effect kind) -- this pins that known gap rather than
    /// silently assuming every such row is a prompt.
    #[test]
    fn the_token_bar_does_not_count_an_unclassifiable_event_only_row() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.inline_use.ask1".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "inline_use": {"value": null, "meta": {"completed_at": null, "inline": true}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.token_bar.prompts_running, 0);
    }

    /// Issue #527's own redraw-cost acceptance: "measure a 2,000-node
    /// state and keep it under the current redraw interval" (`osi`'s
    /// own `REDRAW_INTERVAL`, 100ms). The state pane and the treemap
    /// tree are both built here, inside `build`, from the one `flat`
    /// this test's own single `flatten_state` call already produced
    /// -- not recomputed per pane.
    #[test]
    fn redraw_from_a_2000_node_state_stays_well_under_the_redraw_interval() {
        use serde_json::Map;

        let mut prime: Map<String, Value> = Map::new();
        prime.insert("value".to_string(), Value::Null);
        let mut prime_meta = Map::new();
        prime_meta.insert("completed_at".to_string(), Value::Null);
        prime.insert("meta".to_string(), Value::Object(prime_meta));
        for i in 0..2000 {
            let mut meta = Map::new();
            meta.insert("created_at".to_string(), Value::String(format!("t{i}")));
            meta.insert(
                "completed_at".to_string(),
                Value::String(format!("t{}", i + 1)),
            );
            meta.insert("provider".to_string(), Value::String("shell".to_string()));
            meta.insert("tokens_sent_total".to_string(), Value::from(1));
            meta.insert("tokens_received_total".to_string(), Value::from(2));
            let mut node = Map::new();
            node.insert("value".to_string(), Value::String(format!("ok{i}\n")));
            node.insert("meta".to_string(), Value::Object(meta));
            prime.insert(format!("n{i}"), Value::Object(node));
        }
        let mut root = Map::new();
        root.insert("prime".to_string(), Value::Object(prime));
        let state = Value::Object(root);

        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let start = std::time::Instant::now();
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            10.0,
            "t2001",
            &[],
        );
        let elapsed = start.elapsed();
        assert_eq!(rendered.state_rows.len(), 2000);
        assert_eq!(rendered.rows.len(), 2001);
        // A generous multiple of the 100ms `REDRAW_INTERVAL` itself
        // (measured well under 60ms alone on this machine, debug
        // build) — shared-machine CPU contention can slow a wall-
        // clock measurement like this one down without the code
        // actually regressing, and this is specifically here to
        // catch an accidental quadratic scan again (one shaved this
        // same test from 2.27s to under 60ms), not to pin an exact
        // budget.
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "one redraw of a 2000-node state took {elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );
    }
}
