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

/// One `state` pane entry (DESIGN.md §6.3, issue #527): one of
/// `flatten_state`'s own `{value, meta}` paths, most recently updated
/// first. `value_preview` is `None` exactly when there is nothing
/// useful to show instead of the status glyph: a running node with no
/// value yet, or (regardless of status) once `error_preview` already
/// has something to show in its place.
#[derive(Debug, Clone, PartialEq)]
pub struct StateRow {
    /// Relative to `prime` (`"summarize"`, `"loop.iter_2.step"`) —
    /// `prime` itself is never one of these rows.
    pub path: String,
    pub status: RowStatus,
    pub value_preview: Option<String>,
    pub error_preview: Option<String>,
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
    /// estimate from (the orchestrator's own finding: a tree-loop pass
    /// or a `use` child's prompt never appears in state while it runs
    /// at all, DESIGN.md §1.4) — `estimated_input` then only counts
    /// the running prompts it actually has text for, so a front end
    /// shows it as a lower bound (`≈52+`), not an exact total.
    pub estimated_input_is_lower_bound: bool,
    pub prompts_running: u64,
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
        state,
        process,
        elapsed_s,
        plan,
        &flat,
        &statuses,
        &all_paths,
    );
    let rows = build_rows(plan, &flat, &statuses, &all_paths, now);
    let state_rows = build_state_rows(&flat, &statuses);
    let treemap = crate::treemap::build_tree("prime", &flat, &statuses);
    // The orchestrator's own finding: a tree-loop pass's prompt, and
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

/// The state pane's own rows (DESIGN.md §6.3, issue #527): every
/// path `flatten_state` returns, except `prime` itself, most recently
/// updated first (§2.1's own real-instant parsing, never a raw string
/// compare), ties broken by path.
fn build_state_rows(
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
) -> Vec<StateRow> {
    let mut entries: Vec<(&String, &NodeMeta)> = flat
        .iter()
        .filter(|(path, _)| path.as_str() != "prime")
        .collect();
    entries.sort_by(|(path_a, node_a), (path_b, node_b)| {
        let instant_a = node_a
            .completed_at
            .as_deref()
            .or(node_a.created_at.as_deref())
            .and_then(crate::diff::parse_instant);
        let instant_b = node_b
            .completed_at
            .as_deref()
            .or(node_b.created_at.as_deref())
            .and_then(crate::diff::parse_instant);
        match (instant_a, instant_b) {
            (Some(a), Some(b)) => b.cmp(&a).then_with(|| path_a.cmp(path_b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => path_a.cmp(path_b),
        }
    });

    entries
        .into_iter()
        .map(|(path, node)| {
            let relative = path
                .strip_prefix("prime.")
                .unwrap_or(path.as_str())
                .to_string();
            let status = statuses.get(path.as_str()).cloned().unwrap_or_else(|| {
                let kind = if node.is_running() {
                    StatusKind::Running
                } else if node.is_ok() {
                    StatusKind::Done
                } else {
                    StatusKind::Failed
                };
                RowStatus {
                    kind,
                    skip_reason: None,
                    retrying: false,
                }
            });
            // Finding 9: the first line only — a multi-line error (a
            // shell's own stderr tail, say) would otherwise break the
            // state pane's one-line-per-entry shape just as badly as an
            // untruncated value would.
            let error_preview = node
                .error
                .as_deref()
                .map(|e| e.lines().next().unwrap_or(e))
                .map(|e| preview(e, STATE_PANE_PREVIEW_CHARS));
            let no_value_yet = node.is_running() && node.value.is_null();
            let value_preview = if error_preview.is_some() || no_value_yet {
                None
            } else {
                abbreviated_value(&node.value).map(|v| preview(&v, STATE_PANE_PREVIEW_CHARS))
            };
            let (sent, received) = subtree_tokens(path, flat);
            let tokens = (sent > 0 || received > 0).then_some((sent, received));
            StateRow {
                path: relative,
                status,
                value_preview,
                error_preview,
                tokens,
            }
        })
        .collect()
}

const STATE_PANE_PREVIEW_CHARS: usize = 120;

/// One state node's own value, abbreviated (issue #527): compact JSON
/// for anything but a string, a quoted string for a string (its own
/// newlines escaped so the state pane's one-line-per-entry shape
/// never breaks), `None` for `null` (nothing to show).
fn abbreviated_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(format!("\"{}\"", s.replace('\n', "\\n"))),
        other => Some(
            serde_json::to_string(other)
                .unwrap_or_default()
                .replace('\n', " "),
        ),
    }
}

/// The token bar's "what's in flight" line (DESIGN.md §6.3, issue
/// #527, and the orchestrator's own follow-up finding): the running
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
    all_paths: &BTreeMap<String, ()>,
) -> Header {
    let status = run_status(state, process);

    let (effects_planned, effects_planned_is_lower_bound) = if plan.has_plan() {
        let leaf_paths: std::collections::BTreeSet<&String> = plan.leaf_paths().iter().collect();
        let mut total = 0u64;
        let mut lower_bound = false;
        for leaf in &leaf_paths {
            // The orchestrator's own finding: a `use` leaf's own
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
    // The orchestrator's own invariant: "done" never outruns
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
/// `effects_done` filter, to "done" too (the orchestrator's own
/// finding) -- for a `use` with no *static* children of its own
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
/// A loop-pass container's own status (the orchestrator's own
/// finding), derived from its *leaf* descendants at any depth —
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
    let label = path.rsplit('.').next().unwrap_or(path).to_string();
    let status = statuses.get(path).cloned().unwrap_or_else(|| {
        // The orchestrator's own finding: a loop pass container
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
    /// instead of one -- the shape the orchestrator's own finding
    /// needs: `prime.fan.iter_N` (the pass container) never gets a
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

    /// The orchestrator's own ask: a *chain*-flow loop's own
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
    /// (`fan.iter_0.nap`) must look exactly as it did before this fix
    /// (the orchestrator's own "check that ... single-step passes
    /// still look as before") -- it goes through the identical
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

    /// A real `use: inline:` document (the orchestrator's own
    /// finding): before any of its children have been discovered in
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
    /// once one child has actually landed in state -- the orchestrator's
    /// own ask: "count a use child's effects into planned once they
    /// appear in state, not the use node itself". Both planned and
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
    /// `use` node itself too (the orchestrator's own finding: the two
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
        // `c` is still running (no `completed_at`), so its own update
        // time is `created_at` (00:05), tied with `b`'s own
        // `completed_at` (00:05) -- the tie then breaks by path.
        assert_eq!(paths, vec!["b", "c", "a"]);
    }

    #[test]
    fn state_rows_abbreviate_strings_and_json_and_show_the_glyph_for_a_running_node_with_no_value()
    {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "summarize": {"value": "The report covers everything", "meta": {"created_at": "t0", "completed_at": "t1"}},
            "items": {"value": ["a", "b"], "meta": {"created_at": "t0", "completed_at": "t1"}},
            "pending": {"value": null, "meta": {"created_at": "t2", "completed_at": null}}
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
        let summarize = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "summarize")
            .unwrap();
        assert_eq!(
            summarize.value_preview.as_deref(),
            Some("\"The report covers everything\"")
        );
        let items = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "items")
            .unwrap();
        assert_eq!(items.value_preview.as_deref(), Some(r#"["a","b"]"#));
        let pending = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "pending")
            .unwrap();
        assert_eq!(
            pending.value_preview, None,
            "a running node with no value yet shows the glyph instead"
        );
        assert_eq!(pending.status.kind, StatusKind::Running);
    }

    #[test]
    fn a_failed_state_row_shows_the_start_of_its_error_and_no_value() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "bad": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1): ls: no such file"}}
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
        let bad = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "bad")
            .unwrap();
        assert_eq!(
            bad.error_preview.as_deref(),
            Some("/bin/ls failed (exit 1): ls: no such file")
        );
        assert_eq!(bad.value_preview, None);
    }

    /// Finding 9: only an error's first line — a shell's own
    /// multi-line stderr tail must not break the state pane's
    /// one-line-per-entry shape.
    #[test]
    fn a_multiline_error_only_shows_its_first_line() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "bad": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1)\nls: no such file\nmore detail"}}
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
        let bad = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "bad")
            .unwrap();
        assert_eq!(
            bad.error_preview.as_deref(),
            Some("/bin/ls failed (exit 1)")
        );
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

    /// The orchestrator's own finding: a tree-loop pass's prompt is
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

    /// The orchestrator's own wording finding: when *some* running
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
    /// state and keep it under the current redraw interval" (`osp`'s
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
