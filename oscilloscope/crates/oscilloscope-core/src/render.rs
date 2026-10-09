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
    NodeMeta, ProcessState, RowStatus, RunModel, RunStatus, flatten_state, run_status, run_totals,
};
use crate::observe::duration_seconds;
use crate::plan::{LeafEffectKind, PlanEntryKind, PlanTree};

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
}

/// Everything one redraw needs (§6.3): the header, the plan tree
/// (flattened to depth-first order, a front end collapses a subtree
/// by skipping its descendants) and the log pane's lines so far.
/// `details_for` builds the right pane for whichever row is currently
/// selected — kept separate from `rows` so selecting a different row
/// never needs a fresh `RunModel::observe` call.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderState {
    pub header: Header,
    pub rows: Vec<Row>,
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
    log: &[LogLine],
) -> RenderState {
    let empty = Value::Null;
    let state_ref = state.unwrap_or(&empty);
    let flat = flatten_state(state_ref);
    let statuses = model.observe(state_ref, plan, process);

    let header = build_header(
        document,
        engine,
        model.run_start_id(),
        state,
        process,
        elapsed_s,
        plan,
        &flat,
        &statuses,
    );
    let rows = build_rows(plan, &flat, &statuses);

    RenderState {
        header,
        rows,
        log: log.to_vec(),
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

    let Some(node) = flat.get(path) else {
        return Details {
            path: path.to_string(),
            plan_summary,
            ..Default::default()
        };
    };

    let mut meta_summary = Vec::new();
    let meta = &node.meta;
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
        if let Some(v) = meta.get(key) {
            if !v.is_null() {
                meta_summary.push((key.to_string(), display_value(v)));
            }
        }
    }
    if let Some(args) = meta.pointer("/params_rendered/command") {
        meta_summary.push(("command".to_string(), display_value(args)));
    }
    if let Some(stderr) = meta.get("stderr").and_then(Value::as_str) {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n");
        if !tail.is_empty() {
            meta_summary.push(("stderr".to_string(), tail));
        }
    }

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
    state: Option<&Value>,
    process: ProcessState,
    elapsed_s: f64,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
) -> Header {
    let status = run_status(state, process);

    let (effects_planned, effects_planned_is_lower_bound) = if plan.has_plan() {
        let mut total = 0u64;
        let mut lower_bound = false;
        for leaf in plan
            .leaf_paths()
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let (passes, leaf_lower_bound) = leaf_pass_count(leaf, flat);
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
            matches!(
                status.kind,
                crate::model::StatusKind::Done
                    | crate::model::StatusKind::Failed
                    | crate::model::StatusKind::FailedHandled
            ) && row_kind_for(path, plan, flat.get(path.as_str())).is_leaf()
        })
        .count() as u64;

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
            flat.values().fold((0, 0), |(sent, recv), node| {
                let s = node
                    .meta
                    .get("tokens_sent")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let r = node
                    .meta
                    .get("tokens_received")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
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

    Header {
        document: document.to_string(),
        engine: engine.to_string(),
        run_id: run_id.map(str::to_string),
        status,
        elapsed_s,
        effects_done,
        effects_planned,
        effects_planned_is_lower_bound,
        tokens_sent,
        tokens_received,
        innermost_loop_eta_s,
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
fn leaf_pass_count(leaf_path: &str, flat: &BTreeMap<String, NodeMeta>) -> (u64, bool) {
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

/// Every immediate child segment under `parent` that appears in
/// `paths`, in the order children should render: a loop pass
/// (`iter_N`) sorts by its own numeric index first (so passes always
/// read in order regardless of when osp happened to observe them);
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
    for path in paths.keys() {
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

fn earliest_created_at(
    prefix_path: &str,
    paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
) -> Option<String> {
    let prefix = format!("{prefix_path}.");
    flat.iter()
        .filter(|(p, _)| p.as_str() == prefix_path || p.starts_with(&prefix))
        .filter(|(p, _)| paths.contains_key(p.as_str()))
        .filter_map(|(_, node)| node.created_at.clone())
        .min()
}

fn build_rows(
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
) -> Vec<Row> {
    let mut all_paths: BTreeMap<String, ()> = BTreeMap::new();
    for path in statuses.keys().chain(flat.keys()) {
        all_paths.insert(path.clone(), ());
    }
    all_paths.insert("prime".to_string(), ());

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
        &all_paths,
        flat,
        statuses,
        &plan_order,
        plan,
        &default_status,
        &mut rows,
    );
    rows
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
    out: &mut Vec<Row>,
) {
    let node = flat.get(path);
    let kind = row_kind_for(path, plan, node);
    let label = path.rsplit('.').next().unwrap_or(path).to_string();
    let status = statuses
        .get(path)
        .cloned()
        .unwrap_or_else(|| default_status.clone());
    let duration_s = node.and_then(|n| match (&n.created_at, &n.completed_at) {
        (Some(start), Some(end)) => duration_seconds(start, end),
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
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            &[],
        );
        let order: Vec<&str> = rendered
            .rows
            .iter()
            .filter(|r| r.depth == 2)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(order, vec!["iter_0", "iter_1"]);
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
            &[],
        );
        assert_eq!(rendered.header.effects_planned, Some(1));
        assert!(rendered.header.effects_planned_is_lower_bound);
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
}
