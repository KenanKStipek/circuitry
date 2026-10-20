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

/// A row's own effect type (§6.3's dim provider/model column, and the
/// details pane's "type"): a leaf's own kind when the plan or the
/// node's own `meta` shape can tell, a container kind for everything
/// with children, `Unknown` for an observed-only path (no plan, no
/// shape clue yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    #[default]
    Unknown,
    /// A [`crate::fold::fold_rows`] summary row standing in for two
    /// or more merged quick steps -- never produced anywhere else, so
    /// a caller can always tell a summary row apart from a real one
    /// without parsing its label.
    QuickSteps,
    /// A report's own state node (#605/#610) -- named reports only;
    /// an unnamed one writes no node at all, so there is nothing for
    /// this to ever match. Never a row in the orchestration tree
    /// (`RunModel::observe` already left it out of `statuses`
    /// entirely), but still a real row in the *state* pane, where
    /// its own details apply.
    Report,
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
            RowKind::QuickSteps => "quick steps",
            RowKind::Report => "report",
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
    /// describes for these. The container signals (`progress_total`/
    /// `each_in_path` for a loop, `branch`/`condition_result` for an
    /// `if`, `flow`/`mode` for a bare container -- the same keys
    /// `diff::is_container` checks) all win over `meta.adapter`,
    /// whether or not the node has children of its own: an `if`/loop
    /// in model mode writes `adapter`/`model` alongside its own
    /// condition/progress keys as the record of a real model call, and
    /// a `dynamic` writes the scope's effective adapter/model onto its
    /// own node too (as does the root) -- so a childless container
    /// (an else branch with no `else`, a loop that stopped before its
    /// first pass, or a live snapshot caught between a container's own
    /// write and its first child's) must not read as `Prompt` either
    /// (#616). `has_children` (any descendant of its own in the
    /// flattened state) still falls back to a bare container when none
    /// of the condition/progress keys apply -- a container whose kind
    /// those keys can't name, same as before.
    fn from_meta_shape(node: &NodeMeta, has_children: bool) -> Self {
        let meta = &node.meta;
        if meta.get("provider").is_some() {
            RowKind::Tool
        } else if meta.get("orchestration").is_some() || meta.get("resolved_path").is_some() {
            RowKind::Use
        } else if node.progress_total.is_some() || meta.get("each_in_path").is_some() {
            RowKind::Loop
        } else if node.branch.is_some() || meta.get("condition_result").is_some() {
            RowKind::If
        } else if has_children || meta.get("flow").is_some() || meta.get("mode").is_some() {
            RowKind::Unknown
        } else if meta.get("adapter").is_some() {
            RowKind::Prompt
        } else {
            RowKind::Unknown
        }
    }

    pub(crate) fn is_leaf(self) -> bool {
        matches!(
            self,
            RowKind::Tool | RowKind::Prompt | RowKind::Use | RowKind::Yield | RowKind::Reflector
        )
    }

    /// Whether a path of this kind gets its own, separate `{value,
    /// meta}` state node once it runs, and so its own `on_effect_
    /// start`/`on_effect_complete` pair -- every leaf, the document
    /// root, every *named* `if`/loop, every `dynamic`, and every
    /// `TryFinally`-shaped op. `runtime.last_run.totals.effects_run`
    /// (DESIGN.md §1.2: "every completed node, the root and the
    /// containers included") counts exactly this set, not only
    /// leaves -- `PlanTree::countable_paths`'s own doc comment has
    /// the reasoning for excluding a transparent `if`/loop, the one
    /// container kind with no node of its own.
    fn counts_toward_effects_run(self) -> bool {
        self.is_leaf()
            || matches!(
                self,
                RowKind::Root
                    | RowKind::If
                    | RowKind::Loop
                    | RowKind::Dynamic
                    | RowKind::TryFinally
            )
    }
}

/// Whether `value` is a report's own state value (#605): `{message:
/// str, level: "info"|"warning", data: <any>}`, written only by
/// `Store::fire_report`/`exec::report`'s own named-report branch --
/// distinctive enough that nothing else this codebase writes to a
/// node's own `value` could ever be mistaken for one.
pub(crate) fn is_report_value(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("message").is_some_and(Value::is_string)
        && obj
            .get("level")
            .and_then(Value::as_str)
            .is_some_and(|l| l == "info" || l == "warning")
        && obj.contains_key("data")
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

/// The header line (§6.3). The whole-run ETA (issue #649) comes
/// from history, once there are 3 or more ok runs of this document's
/// own hash (`history_eta_ts`/`history_eta_past_usual`, left at
/// their own "no history" defaults by `build`/`build_header`
/// themselves — only `oscilloscope`'s own history wiring, which
/// alone knows where a document's history lives, ever sets them to
/// anything else, as a direct, after-the-fact write onto the
/// `Header` this module already built). Without one, the header
/// falls back to the innermost *running* loop's own ETA instead
/// (plans have data-dependent loops, so that one is never a whole-
/// run figure).
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
    /// The whole-run ETA from history (issue #649), as an absolute
    /// clock time — `None` either because there isn't enough
    /// history yet (the header then falls back to
    /// `innermost_loop_eta_ts`) or because [`Header::history_eta_
    /// past_usual`] is set instead.
    pub history_eta_ts: Option<String>,
    /// Set once the run is past its own usual (p90) end, with enough
    /// history to know one — the header then reads `ETA ?` rather
    /// than falling back to the loop ETA.
    pub history_eta_past_usual: bool,
    /// `history_eta_ts`'s own seconds-from-now, before it was turned
    /// into a clock time — the overview's own `(12m)`. `None`
    /// exactly when `history_eta_ts` is.
    pub history_eta_remaining_s: Option<f64>,
    /// The p10/p90 range behind `history_eta_ts`, as clock times —
    /// the overview's own `usual 11:58–12:09`. Either may be `None`
    /// below the p10/p90 run-count threshold even with
    /// `history_eta_ts` set.
    pub history_eta_low_ts: Option<String>,
    pub history_eta_high_ts: Option<String>,
    /// How far ahead of (negative) or behind (positive) usual pace
    /// the run is at its own anchor step — the overview's own `vs
    /// usual +3m`. `None` before any recorded step has finished.
    pub history_vs_usual_s: Option<f64>,
    /// The usual (p50) whole-run time behind `history_vs_usual_s` —
    /// the overview's own `(p50 45m)`.
    pub history_usual_total_s: Option<f64>,
    /// How many ok runs of the current document hash history has,
    /// whatever their count — the overview's own `history: N of 3
    /// runs` while below the threshold a whole-run ETA itself needs,
    /// including `0` on a document's very first ok run. `None`
    /// exactly when history itself hasn't resolved a document and a
    /// directory to read yet (history off, or a watch still waiting
    /// on its first live-state write) -- the only case this is
    /// never shown at all, as opposed to `Some(0)`.
    pub history_same_hash_count: Option<usize>,
    /// Per-step usual busy time from history (issue #649's now-pane
    /// bar), keyed the same way [`crate::history::HistoryRecord::
    /// steps`] is -- pooled across every ok run of this document
    /// path, any hash ("earlier versions" pooling). Empty when
    /// history is off or there's no document yet.
    pub history_step_usual: std::collections::BTreeMap<String, crate::history::StepStats>,
    /// The latest report seen, once at least one has arrived (#610)
    /// -- a front end shows this in the header's own status-line
    /// slot, in place of `run_id`, for the rest of the run. `None`
    /// before any report at all, in which case the header reads
    /// exactly as it did before #610.
    pub latest_report: Option<HeaderReport>,
    /// Whether the engine process is alive right now (issue #626's
    /// header line: "`♥` shows while the engine process is alive") --
    /// the one osi started (`ProcessState::Running`), or, for `osi
    /// watch`, the pid `--events`' own `run_start` carried, read by
    /// the caller the same way `run_start_pid` already does.
    pub engine_alive: bool,
}

/// [`Header::latest_report`]'s own shape (#610): just enough for the
/// status line (`message`, styled by `level`) -- a report's `data` is
/// a details-pane/log-pane concern only, never the header's.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderReport {
    pub level: crate::observe::ReportLevel,
    pub message: String,
    /// The sighting's own `ts` (#610): carried only so two sightings
    /// with the same level and message still compare unequal --
    /// `changes::header_changes` blames a new report for its own
    /// highlight key purely by `latest_report` changing, and a repeat
    /// report (the same "chain pass" message on every loop pass, for
    /// instance) must still flash every time it actually arrives.
    pub ts: String,
}

/// The selected row's own details (§6.3's right pane). `prompt_sent`
/// and `value` are truncated to [`DETAILS_PREVIEW_CHARS`]; a front
/// end shows them in full on request (the `v` key).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details {
    pub path: String,
    /// The row's own kind (#610): used to be a separate parameter
    /// every caller of `details_lines`/`draw_details` had to thread
    /// through on its own, kept in sync with whichever row happened
    /// to be selected by hand -- now computed once, here, from the
    /// exact same `path` this whole struct is already built from.
    pub kind: RowKind,
    /// The details pane's own title (issue #613's pass labels:
    /// `pass 3 of 8 (iter_2)` belongs here, in the pane's
    /// title, not in `plan_summary`'s `plan` line) -- `None` for
    /// every path that isn't a loop pass, so the front end falls
    /// back to its own plain " details ".
    pub title: Option<String>,
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
    /// of their own -- live agent token usage (#612) is a later lane.
    pub agents_running: u64,
}

/// `draw_token_bar`'s own second line, as plain text (issue #626):
/// pulled out so the overview's cost card (`crate::screens::overview`)
/// can show the same "what's in flight" line without reimplementing
/// it -- `nothing in flight`, `N prompt(s)/agent(s) in flight`, or
/// `now in ~N   (...)` once a running prompt has a token estimate.
pub fn token_bar_in_flight_line(bar: &TokenBar) -> String {
    if bar.prompts_running == 0 && bar.agents_running == 0 {
        return "nothing in flight".to_string();
    }
    let mut parts = Vec::new();
    if bar.prompts_running > 0 {
        let plural = if bar.prompts_running == 1 { "" } else { "s" };
        parts.push(format!("{} prompt{plural}", bar.prompts_running));
    }
    if bar.agents_running > 0 {
        let plural = if bar.agents_running == 1 { "" } else { "s" };
        parts.push(format!("{} agent{plural}", bar.agents_running));
    }
    let in_flight = format!("{} in flight", parts.join(" · "));
    if bar.prompts_running == 0 {
        return in_flight;
    }
    // Issue #631: while prompts are in flight but no estimate is
    // available at all yet (not even a lower bound), say only what's
    // actually known ("3 prompts in flight"), not "now in ?" --
    // the estimate is added back in once one actually exists.
    if bar.estimated_input_is_lower_bound && bar.estimated_input == 0 {
        return in_flight;
    }
    let estimate = if bar.estimated_input_is_lower_bound {
        format!("≈{}+", crate::format::format_tokens(bar.estimated_input))
    } else {
        format!("≈{}", crate::format::format_tokens(bar.estimated_input))
    };
    format!("now in {estimate}   ({in_flight})")
}

/// Everything one redraw needs (§6.3): the header, the plan tree
/// (flattened to depth-first order, a front end collapses a subtree
/// by skipping its descendants), the state pane's rows, the token
/// bar, and the log pane's lines so far. `details_for` builds the
/// right pane for whichever row is currently selected — kept separate
/// from `rows` so selecting a different row never needs a fresh
/// `RunModel::observe` call. The Cost screen's own by-store treemap
/// (issue #632) is deliberately not part of this: it is built only
/// while that screen is actually on display (`main.rs`'s own
/// `build_cost_screen_if_active`), not on every redraw regardless of
/// which screen is showing, the way the old `tokens by key` treemap
/// screen 2 drew was.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderState {
    pub header: Header,
    pub rows: Vec<Row>,
    /// The same rows as `rows`, before `build_folded` ever folds
    /// them -- `build` alone sets this equal
    /// to `rows` itself, since there is nothing to fold yet.
    /// `changes::diff` reads this, not `rows`, so toggling folding or
    /// expanding a quick-steps group -- which can make a row appear
    /// or disappear from `rows` with nothing underneath it actually
    /// having changed -- never highlights anything by itself.
    pub unfolded_rows: Vec<Row>,
    pub state_rows: Vec<StateRow>,
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
    let statuses = model.observe(state_ref, plan, process.clone());
    // Every path either side of a run has ever heard of (hoisted here,
    // issue/finding on `use` accounting): the header's own
    // planned/done counts need the identical view `build_rows`/
    // `child_segments` already use to discover a `use` child's
    // children once they land in state -- building it twice, once per
    // caller, would both cost the walk twice and risk the two counts
    // disagreeing about what "discovered" means.
    let all_paths = all_observed_paths(&flat, &statuses, plan, model);

    let header = build_header(
        document, engine, model, state, process, elapsed_s, now, plan, &flat, &statuses, &all_paths,
    );
    let rows = build_rows(
        plan,
        &flat,
        &statuses,
        &all_paths,
        now,
        model,
        header.run_end_ts.as_deref(),
    );
    let state_rows = build_state_rows(
        plan,
        &flat,
        &statuses,
        model,
        header.run_end_ts.as_deref(),
        now,
    );
    // A tree-loop pass's prompt, and
    // (on cof) a `use` child's, never appear in `flat` while they run
    // at all (DESIGN.md §1.4) -- only `rows` (built from `statuses`,
    // which events confirm `Running` for a path `flat` has never even
    // heard of) can see them at all, so the running-prompt count reads
    // `rows`, not `flat` alone.
    let token_bar = build_token_bar(&rows, &flat, plan, now);

    RenderState {
        header,
        unfolded_rows: rows.clone(),
        rows,
        state_rows,
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
/// for a loop. `None` when there's nothing to show. `kind` (#616)
/// gates the model on `Prompt`/`Reflector`, the same gate
/// `walk_rows`/`details_for` use for the orchestration pane and the
/// details pane: both engines write the scope's effective model into
/// a container's own node too (the root, `dynamic`, a model-mode
/// `if`/loop), so only a row whose *kind* actually ran a prompt shows
/// one, whatever its `meta` says -- `has_children` alone missed a
/// childless container carrying the same keys.
fn state_row_dim(
    node: &NodeMeta,
    kind: RowKind,
    status_kind: StatusKind,
    event_duration_s: Option<f64>,
    run_end_ts: Option<&str>,
    now: &str,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(model) = matches!(kind, RowKind::Prompt | RowKind::Reflector)
        .then(|| node.meta.get("model").and_then(Value::as_str))
        .flatten()
    {
        parts.push(model.to_string());
    } else if let Some(provider) = node.meta.get("provider").and_then(Value::as_str) {
        parts.push(provider.to_string());
    }
    // Issue #613: a running row shows its own elapsed so far too, not
    // only a finished one's final duration. `changes::diff` never
    // compares a running row's `dim` at all (below), so this ticking
    // every rebuild never counts as a change on its own. A cancelled
    // or aborted row -- no `completed_at` of its own, but not running
    // either -- must not keep ticking up to `now` once the run has
    // ended: it measures up to `now` only while still running
    // (`Running`/`LikelyRunning`/`RunningOrQueued`), else it prefers
    // an `--events` end duration, else the run's own end instant, else
    // shows no duration at all.
    match (&node.created_at, &node.completed_at) {
        (Some(created), Some(completed)) => {
            if let Some(secs) = duration_seconds(created, completed) {
                parts.push(crate::format::format_duration(secs));
            }
        }
        (Some(created), None) => {
            let is_running = matches!(
                status_kind,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            );
            let secs = if is_running {
                duration_seconds(created, now)
            } else {
                event_duration_s
                    .or_else(|| run_end_ts.and_then(|end| duration_seconds(created, end)))
            };
            if let Some(secs) = secs {
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
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    model: &RunModel,
    run_end_ts: Option<&str>,
    now: &str,
) -> Vec<StateRow> {
    let tree = build_state_tree(flat);
    let mut out = Vec::new();
    flatten_state_tree(
        plan, &tree, "prime", 0, flat, statuses, model, run_end_ts, now, &mut out,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn flatten_state_tree(
    plan: &PlanTree,
    nodes: &BTreeMap<String, StateTreeNode>,
    parent_path: &str,
    depth: usize,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    model: &RunModel,
    run_end_ts: Option<&str>,
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
        let has_children = !node.children.is_empty();
        let kind = row_kind_for_with_has_children(&full_path, plan, flat, has_children);
        let dim = node.own.and_then(|n| {
            state_row_dim(
                n,
                kind,
                status.kind,
                model.event_duration_s(&full_path),
                run_end_ts,
                now,
            )
        });
        out.push(StateRow {
            path: full_path
                .strip_prefix("prime.")
                .unwrap_or(&full_path)
                .to_string(),
            depth,
            key: key.clone(),
            has_children,
            status,
            dim,
            tokens,
        });
        flatten_state_tree(
            plan,
            &node.children,
            &full_path,
            depth + 1,
            flat,
            statuses,
            model,
            run_end_ts,
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
///
/// A tree-loop pass and a `use` child have no node in `flat` at all
/// while they run on `cof`, so an `agent` tool there is counted from
/// `plan` instead: a leaf's own `provider` string, recorded by
/// `PlanTree` from the compiled `ToolOp` itself.
fn build_token_bar(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    now: &str,
) -> TokenBar {
    let mut estimated_input = 0u64;
    let mut estimated_input_is_lower_bound = false;
    let mut prompts_running = 0u64;
    let mut agents_running = 0u64;
    let mut counted_agent_paths: std::collections::BTreeSet<&str> =
        std::collections::BTreeSet::new();
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
            // read from state when a node exists, else from the plan's
            // own `PlanEntry` (recorded from the compiled `ToolOp`,
            // for a row with no state node yet). Agents get no token
            // estimate.
            let provider = node
                .and_then(|n| n.meta.get("provider"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    plan.match_path(&row.path)
                        .and_then(|m| m.entries.first())
                        .and_then(|e| e.provider.clone())
                });
            if provider.is_some_and(|p| p.trim().eq_ignore_ascii_case("agent")) {
                agents_running += 1;
                counted_agent_paths.insert(row.path.as_str());
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
    // Issue #637's heartbeat rule: a node with `meta.live` whose
    // `updated_at` is within 5 seconds of `now` counts as running
    // even with no `--events` to confirm it -- this reads `flat`
    // alone, no `rows`/`--events` involved at all, so it also catches
    // a step `rows` doesn't yet classify as `Running` for some other
    // reason. Never double-counts a path the loop above already
    // counted; never counts a node whose own `meta.live` has gone
    // stale (an interrupted run's own leftover, DESIGN.md), since a
    // stale summary's `updated_at` has stopped moving and so is never
    // within 5 seconds of a `now` sampled well afterwards.
    for (path, node) in flat {
        if counted_agent_paths.contains(path.as_str()) {
            continue;
        }
        if node.completed_at.is_some() {
            continue;
        }
        if node.live.as_ref().is_some_and(|live| live.is_recent(now)) {
            agents_running += 1;
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
/// highlighted, not for every row on every redraw. `model` is only
/// needed for #610's own additions (the root's own `run_id`, and a
/// named report's latest event sighting) -- every other field here
/// comes from `plan`/`state` exactly as before.
pub fn details_for(
    path: &str,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
) -> Details {
    let empty = Value::Null;
    let flat = flatten_state(state.unwrap_or(&empty));
    let kind = row_kind_for(path, plan, &flat);
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
    // Issue #613's pass labels: the details
    // *title* shows both the orchestration pane's own `pass N of M`
    // and the state pane's own real key (`iter_N`) together, for any
    // path shaped like a loop pass -- regardless of whether the plan
    // matched it to the enclosing loop's own entry (which knows
    // nothing about "pass" at all) or not. `plan_summary`'s own
    // `plan` line is untouched by this -- it still shows the plan
    // entry's own kind/name, same as for every other row.
    let title = {
        let last = path.rsplit('.').next().unwrap_or(path);
        pass_index(last).map(|n| {
            let loop_path = path.rsplit_once('.').map(|(p, _)| p);
            let total = loop_path
                .and_then(|p| flat.get(p))
                .and_then(|loop_node| loop_node.progress_total);
            match total {
                Some(total) => format!("pass {} of {total} ({last})", n + 1),
                None => format!("pass {} ({last})", n + 1),
            }
        })
    };

    let Some(node) = flat.get(path) else {
        return Details {
            path: path.to_string(),
            kind,
            title,
            plan_summary,
            meta_summary: root_run_id_summary(path, model),
            ..Default::default()
        };
    };

    if kind == RowKind::Report {
        return report_details(path, node, model, title);
    }

    let meta = &node.meta;
    // Issue #535: a node with `prompt_sent` gets the `sent`/`received`
    // blocks below instead -- their headers already carry the token
    // counts, so the plain meta list drops them to avoid saying the
    // same thing twice.
    let has_prompt_sent = meta.get("prompt_sent").and_then(Value::as_str).is_some();

    let mut meta_summary = root_run_id_summary(path, model);
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
        // #616: `meta.model`/`meta.adapter` only ever name a real
        // prompt call for a kind that actually ran one -- both engines
        // also write the scope's effective adapter/model into a
        // container's own node (the root, `dynamic`, a model-mode
        // `if`/loop), so showing `adapter` alone for such a row would
        // look like a half-hidden model tag rather than a container
        // with none.
        if (key == "model" || key == "adapter")
            && !matches!(kind, RowKind::Prompt | RowKind::Reflector)
        {
            continue;
        }
        if let Some(v) = meta.get(key) {
            if !v.is_null() {
                let text = if key == "tokens_sent" || key == "tokens_received" {
                    v.as_u64()
                        .map(crate::format::format_tokens)
                        .unwrap_or_else(|| display_value(v))
                } else {
                    display_value(v)
                };
                meta_summary.push((key.to_string(), text));
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
        kind,
        title,
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

/// #610: the root row's own `run_id`, from `--events`' own
/// `run_start` (DESIGN.md §3) -- the header no longer shows it once a
/// report has arrived, so this is the only place left to see it,
/// aside from `--log`'s own `■ run started` line. Empty for every
/// path but `prime`, and for `prime` itself before a `run_start` has
/// ever arrived (an engine with no `--events` support, or one that
/// hasn't written one yet).
fn root_run_id_summary(path: &str, model: &RunModel) -> Vec<(String, String)> {
    if path != "prime" {
        return Vec::new();
    }
    model
        .run_start_id()
        .map(|run_id| vec![("run_id".to_string(), run_id.to_string())])
        .unwrap_or_default()
}

/// A named report's own details (#610): `type report`, its
/// `level`, the latest `message` and the clock time it arrived, and
/// `data` as pretty JSON. The latest `--events` sighting for this
/// exact path wins over the state node's own copy wherever it has an
/// answer -- state only ever holds the *first* named report's write
/// at that path (`exec::report`'s own `set_leaf` overwrites it on a
/// later pass, same as any other node, so in practice the two rarely
/// disagree, but events are the more current source when they do).
fn report_details(path: &str, node: &NodeMeta, model: &RunModel, title: Option<String>) -> Details {
    let event = model.report_at(path);
    let state_level = node.value.get("level").and_then(Value::as_str);
    let level = match event.map(|e| e.level) {
        Some(crate::observe::ReportLevel::Warning) => "warning",
        Some(crate::observe::ReportLevel::Info) => "info",
        None => state_level.unwrap_or("info"),
    };
    let message = event
        .map(|e| e.message.clone())
        .or_else(|| {
            node.value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let clock_ts = event
        .map(|e| e.ts.clone())
        .or_else(|| node.completed_at.clone())
        .or_else(|| node.created_at.clone());
    // A named report keeps its full `data` in the shadow state even
    // when the matching event sighting was truncated (the state write
    // and the `--events` line are independent; only the line has a
    // size cap), so the state value is the fallback, not a dead end.
    let data = event
        .and_then(|e| e.data.clone())
        .or_else(|| node.value.get("data").cloned())
        .filter(|d| !d.is_null());
    let data_truncated = data.is_none() && event.is_some_and(|e| e.data_truncated);

    let mut meta_summary = vec![
        ("level".to_string(), level.to_string()),
        ("message".to_string(), message),
    ];
    if let Some(ts) = &clock_ts {
        meta_summary.push(("time".to_string(), ts.clone()));
    }

    let value = if data_truncated {
        Some("(data truncated)".to_string())
    } else {
        data.map(|d| {
            let pretty = serde_json::to_string_pretty(&d).unwrap_or_default();
            preview(&pretty, DETAILS_PREVIEW_CHARS)
        })
    };

    Details {
        path: path.to_string(),
        kind: RowKind::Report,
        title,
        plan_summary: String::new(),
        meta_summary,
        error: None,
        prompt_sent: None,
        value,
        sent: None,
        received: None,
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
    model: &mut RunModel,
    state: Option<&Value>,
    process: ProcessState,
    fallback_elapsed_s: f64,
    now: &str,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    all_paths: &BTreeMap<String, ()>,
) -> Header {
    let engine_alive = matches!(process, ProcessState::Running);
    let status = run_status(state, process.clone());
    let run_id = model.run_start_id().map(str::to_string);

    // Issue #613's "one clock": the run's own start, from the first
    // `--events` `run_start` when one arrived, else the earliest
    // `created_at` anywhere in state -- real instants compared, never
    // raw strings (DESIGN.md's own warning: an event's millisecond-
    // plus-`Z` timestamp and a state microsecond-plus-offset one
    // aren't lexicographically comparable).
    let run_start_ts = model
        .run_start_ts()
        .map(str::to_string)
        .or_else(|| earliest_created_at("prime", all_paths, flat));
    let run_end_ts = resolve_run_end_ts(model, flat, all_paths, process, status, now);
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
        let countable_paths: std::collections::BTreeSet<&String> =
            plan.countable_paths().iter().collect();
        let mut total = 0u64;
        let mut lower_bound = false;
        for path in &countable_paths {
            // A `use` leaf's own
            // children are the real planned units, not the `use` node
            // itself -- counting both (already true for a `path:`
            // child whose own children are separately grafted into
            // `countable_paths`) or neither (an `inline:`/`ref:` child,
            // which has no static plan at all) is what let `done`
            // outrun `planned` once a `use` child's effects actually
            // landed in state.
            if row_kind_for(path, plan, flat) == RowKind::Use {
                let prefix = format!("{path}.");
                let has_static_children = countable_paths.iter().any(|p| p.starts_with(&prefix));
                if has_static_children {
                    // Grafted `use: path:`: its own children are
                    // already separate entries in `countable_paths`,
                    // each counted on their own below.
                    continue;
                }
                let (count, use_lower_bound) =
                    use_leaf_contribution(path, plan, flat, statuses, all_paths);
                total = total.saturating_add(count);
                lower_bound |= use_lower_bound;
                continue;
            }
            let (passes, path_lower_bound) = leaf_pass_count(path, plan, flat);
            total = total.saturating_add(passes);
            lower_bound |= path_lower_bound;
        }
        (Some(total), lower_bound)
    } else {
        (None, false)
    };
    let effects_done_raw = statuses
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
            let kind = row_kind_for(path, plan, flat);
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
            } else if kind == RowKind::Unknown {
                // No plan entry at all (`osi watch`, or an
                // `inline:`/`ref:` use child), and no distinguishing
                // `meta` shape either (`from_meta_shape`'s own
                // catch-all) -- but a node with real children in
                // state is still some container that genuinely ran
                // (only a container ever has them; `RowKind::Use`'s
                // own shape signals, checked above, already cover a
                // `use`), even nameless to `osi`: a bare `dynamic`
                // carries no `meta` shape of its own to guess a
                // kind from at all (DESIGN.md §5). Counting it here
                // is what lets a finished run under no plan match a
                // planned one's own count for a document with a
                // plain `dynamic` in it.
                crate::model::subtree_entries(path.as_str(), flat)
                    .any(|(p, _)| p.as_str() != path.as_str())
            } else {
                kind.counts_toward_effects_run()
            }
        })
        .count() as u64;
    // Once the engine has deterministically ended the run (issue
    // #620) -- `Ok`, `Failed`, or a single-signal `Cancelled`, every
    // outcome DESIGN.md §2.1 rule 7 reaches from `runtime.last_run.
    // completed_at` landing in a real `--out` write of its own --
    // nothing more will ever run, so "planned" can only mean "what
    // actually ran": an untaken `if` branch, an empty loop pass, a
    // chain stopped by an earlier failure, a disabled container's own
    // un-run children, and the rest of DESIGN.md §6.3's "effects done
    // out of planned" all leave the planned total the same way.
    // `Aborted` is excluded, whether a second signal left no final
    // write at all (DESIGN.md §1.1) or a pre-execution validation
    // failure never wrote one in the first place (`run_status`'s own
    // "no ended state" branch): there is no `effects_run` to match
    // either way, and real work may still be left incomplete.
    let run_has_ended = state.is_some_and(crate::model::run_ended)
        && matches!(
            status,
            RunStatus::Ok | RunStatus::Failed | RunStatus::Cancelled
        );
    // The engine's own `runtime.last_run.totals.effects_run` is
    // recorded live as each effect completes (DESIGN.md §6.3,
    // `_TotalsAccumulator.observe`/`Totals::observe_complete`), not by
    // walking the final state -- so once it's present it's the exact
    // count on both sides, including the two shapes `effects_done_raw`
    // can't see: a declared-outputs `use` child, whose own effects
    // never land in the final state tree at all even though they ran,
    // and an *unnamed* loop with more than one pass, whose every pass
    // overwrites the very same state path so only the last survives
    // for a walk to find. Walking the state tree stays the fallback
    // only for an ended run with no totals to read at all (a
    // synthetic state, as in this module's own unit tests).
    let effects_run_from_totals = state
        .and_then(run_totals)
        .and_then(|totals| totals.get("effects_run").and_then(Value::as_u64));
    let (effects_planned, effects_planned_is_lower_bound, effects_done) = if run_has_ended {
        let count = effects_run_from_totals.unwrap_or(effects_done_raw);
        (Some(count), false, count)
    } else {
        // Invariant: "done" never outruns
        // "planned" -- true by construction once a `use` child's effects
        // are counted the same way on both sides above, but kept as an
        // explicit floor rather than trusting that invariant silently,
        // since a document this walker has never seen could still surprise
        // it.
        let done =
            effects_planned.map_or(effects_done_raw, |planned| effects_done_raw.min(planned));
        (effects_planned, effects_planned_is_lower_bound, done)
    };

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

    let latest_report = model.latest_report().map(|r| HeaderReport {
        level: r.level,
        message: r.message.clone(),
        ts: r.ts.clone(),
    });

    Header {
        document: document.to_string(),
        engine: engine.to_string(),
        run_id,
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
        // Issue #649: `build`/`build_header` have no history of
        // their own to read (only `oscilloscope`'s own history
        // wiring knows where a document's history lives) -- these
        // always start at "no history" and are set afterward, by the
        // caller, directly on the `Header` this returns.
        history_eta_ts: None,
        history_eta_past_usual: false,
        history_eta_remaining_s: None,
        history_eta_low_ts: None,
        history_eta_high_ts: None,
        history_vs_usual_s: None,
        history_usual_total_s: None,
        history_same_hash_count: None,
        history_step_usual: Default::default(),
        latest_report,
        engine_alive,
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

fn row_kind_for(path: &str, plan: &PlanTree, flat: &BTreeMap<String, NodeMeta>) -> RowKind {
    let has_children = crate::model::subtree_entries(path, flat).any(|(p, _)| p != path);
    row_kind_for_with_has_children(path, plan, flat, has_children)
}

/// [`row_kind_for`], given a caller's own already-known `has_children`
/// instead of recomputing it with [`crate::model::subtree_entries`] --
/// the state pane already has this cheaply from its own tree walk
/// (`build_state_tree`), and calling `subtree_entries` again for every
/// row of a deep state turned the whole pane's build quadratic (the
/// same scan this file's `redraw_from_a_2000_node_state_...` test
/// exists to catch).
fn row_kind_for_with_has_children(
    path: &str,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    has_children: bool,
) -> RowKind {
    if path == "prime" {
        return RowKind::Root;
    }
    // The plan wins first: a report never has a plan entry of its
    // own at all (`plan::record_op_entry`'s own early return), so
    // checking it ahead of the report-shape guess below only ever
    // skips paths a report could never be at anyway -- but a planned
    // tool or prompt whose own output happens to match the report
    // shape (`{message: str, level: info|warning, data}`) must still
    // read as its real plan kind, not get misclassified as `Report`
    // (#610).
    if let Some(entry) = plan.match_path(path).and_then(|m| m.entries.first()) {
        return RowKind::from_plan_entry(&entry.kind);
    }
    let node = flat.get(path);
    if node.is_some_and(|n| is_report_value(&n.value)) {
        return RowKind::Report;
    }
    if let Some(node) = node {
        return RowKind::from_meta_shape(node, has_children);
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
        .filter(|(p, ())| row_kind_for(p, plan, flat).is_leaf())
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
            // Compared as real instants when both parse, never a raw
            // string compare: an engine's own millisecond timestamp
            // and a bare-seconds one aren't lexicographically
            // comparable at all (the same class of bug `diff::
            // sort_log_lines` guards against) -- `2026-01-01T00:00:
            // 00Z` is earlier than `...00.200Z`, but sorts *after* it
            // as a string, since `.` (0x2E) sorts below `Z` (0x5A).
            (Some(a), Some(b)) => {
                return match (
                    chrono::DateTime::parse_from_rfc3339(&a),
                    chrono::DateTime::parse_from_rfc3339(&b),
                ) {
                    (Ok(ia), Ok(ib)) => ia.cmp(&ib),
                    _ => a.cmp(&b),
                };
            }
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

/// A run that ended with no `run_end` event and no
/// `prime.meta.completed_at` of its own (an abort -- the engine was
/// killed) still has some real instant to end `elapsed` at, rather
/// than leaving it ticking up to `now` forever: the latest of every
/// node's own `completed_at`, every node's own `created_at` (so a
/// step that was killed, with no `completed_at` of its own, still
/// counts -- a step that finished earlier must not win over one still
/// running when the process died), and the latest `start`/`end`
/// event `ts` seen.
fn latest_known_instant(
    paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
) -> Option<String> {
    let mut candidates: Vec<&str> = Vec::new();
    for (p, ()) in crate::model::subtree_entries("prime", paths) {
        if let Some(node) = flat.get(p.as_str()) {
            if let Some(ts) = node.completed_at.as_deref() {
                candidates.push(ts);
            }
            if let Some(ts) = node.created_at.as_deref() {
                candidates.push(ts);
            }
        }
    }
    if let Some(ts) = model.latest_event_ts() {
        candidates.push(ts);
    }
    candidates
        .into_iter()
        .filter_map(|ts| {
            let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
            Some((parsed, ts.to_string()))
        })
        .max_by_key(|(parsed, _)| *parsed)
        .map(|(_, ts)| ts)
}

/// The header's own `run_end_ts` (DESIGN.md's "one clock"): the first
/// known of `run_end`'s own `ts`, `prime.meta.completed_at`, the time
/// `osi run` itself saw the engine process exit
/// ([`ProcessState::exited_at`] -- `None` for `osi watch`, which owns
/// no process), and [`latest_known_instant`]. A run still running
/// never has one (`elapsed` measures up to `now` instead, in the
/// caller). A run that's ended with none of the above known at all --
/// nothing ever started, completed or was reported by `--events`,
/// and no process-exit instant either -- freezes `elapsed` at `now`
/// the first time this is reached, rather than counting on forever.
fn resolve_run_end_ts(
    model: &mut RunModel,
    flat: &BTreeMap<String, NodeMeta>,
    all_paths: &BTreeMap<String, ()>,
    process: ProcessState,
    status: RunStatus,
    now: &str,
) -> Option<String> {
    if status == RunStatus::Running {
        return None;
    }
    let known = model
        .run_end_ts()
        .map(str::to_string)
        .or_else(|| flat.get("prime").and_then(|n| n.completed_at.clone()))
        .or_else(|| process.exited_at().map(str::to_string))
        .or_else(|| latest_known_instant(all_paths, flat, model));
    Some(known.unwrap_or_else(|| model.freeze_end_ts(now).to_string()))
}

fn earliest_created_at(
    prefix_path: &str,
    paths: &BTreeMap<String, ()>,
    flat: &BTreeMap<String, NodeMeta>,
) -> Option<String> {
    // Compared as real instants when both sides parse as RFC-3339,
    // not a blind raw-string compare --
    // real `created_at`s all share one consistent format within a
    // single engine's own state, so that promise is never actually
    // load-bearing there, but this is the one place it's made
    // explicit rather than assumed. Falls back to the raw strings'
    // own order when either side doesn't parse, which keeps every
    // existing fixture's own placeholder timestamps (`"t0"`, `"t1"`,
    // ...) ordered exactly as before.
    crate::model::subtree_entries(prefix_path, paths)
        .filter_map(|(p, ())| flat.get(p.as_str()))
        .filter_map(|node| node.created_at.clone())
        .min_by(|a, b| {
            match (
                chrono::DateTime::parse_from_rfc3339(a),
                chrono::DateTime::parse_from_rfc3339(b),
            ) {
                (Ok(pa), Ok(pb)) => pa.cmp(&pb),
                _ => a.cmp(b),
            }
        })
}

/// Every path `flat` or `statuses` has an entry for, plus `prime`
/// itself (which may have neither, before the first write) — shared
/// by `build_header`'s own planned/done counts and `build_rows`'s own
/// tree walk, so both agree on what a `use` node's own discovered
/// children are.
fn all_observed_paths(
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    plan: &PlanTree,
    model: &RunModel,
) -> BTreeMap<String, ()> {
    let mut all_paths: BTreeMap<String, ()> = BTreeMap::new();
    // #610: `statuses` (`RunModel::observe`) already left a report
    // path out, but `flat` (every `{value, meta}` node, report state
    // nodes included) hasn't -- without the same exclusion here, a
    // named report's own node would still seed a row for `walk_rows`
    // to walk, just with a `Pending` status instead of the `Done` it
    // used to show.
    for path in statuses.keys().chain(flat.keys()) {
        // See `RunModel::observe`'s own matching guard: `prime` is
        // never treated as report-only, and the no-plan fallback only
        // trusts `--events`' own report paths where the plan has no
        // real entry there and the node's value has the report shape.
        let fallback_report = flat
            .get(path)
            .is_some_and(|node| model.is_report_event_path(path, plan, &node.value));
        if plan.is_report_path(path) || (path != "prime" && fallback_report) {
            continue;
        }
        all_paths.insert(path.clone(), ());
    }
    all_paths.insert("prime".to_string(), ());
    all_paths
}

pub(crate) fn build_rows(
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    all_paths: &BTreeMap<String, ()>,
    now: &str,
    model: &RunModel,
    run_end_ts: Option<&str>,
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
        model,
        run_end_ts,
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
        let kind = row_kind_for(descendant, plan, flat);
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
    model: &RunModel,
    run_end_ts: Option<&str>,
    out: &mut Vec<Row>,
) {
    let node = flat.get(path);
    let kind = row_kind_for(path, plan, flat);
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
    // A row no state node exists for yet (a tree-loop pass, or a
    // `use` child) still has a start `ts` and, once finished, an end
    // `ms` from `--events` alone -- used only when there is no node
    // at all, so a node's own `created_at`/`completed_at` always wins
    // once one exists. A node with `created_at` but no `completed_at`
    // measures up to `now` only while its own status is still a
    // running-type one; a cancelled or aborted row (or a `Done` row
    // `--events` has already confirmed, ahead of state) must not keep
    // ticking once the run has ended, so it prefers an `--events` end
    // duration, then the run's own end instant, then no duration at
    // all.
    let is_running_kind = matches!(
        status.kind,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
    );
    let duration_s = node
        .and_then(|n| match (&n.created_at, &n.completed_at) {
            (Some(start), Some(end)) => duration_seconds(start, end),
            (Some(start), None) if is_running_kind => duration_seconds(start, now),
            (Some(start), None) => model
                .event_duration_s(path)
                .or_else(|| run_end_ts.and_then(|end| duration_seconds(start, end))),
            _ => None,
        })
        .or_else(|| match status.kind {
            StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => model
                .event_started_at(path)
                .and_then(|start| duration_seconds(start, now)),
            StatusKind::Done | StatusKind::Failed | StatusKind::FailedHandled => {
                model.event_duration_s(path)
            }
            _ => None,
        });
    let loop_progress = node.and_then(|n| {
        n.progress_done.map(|done| LoopProgress {
            done,
            total: n.progress_total,
            eta_s: n.progress_eta_s,
        })
    });
    // #616: `meta.model` names a row's own model only for a kind that
    // actually runs a prompt (`Prompt`/`Reflector`) -- both engines
    // also write the scope's effective adapter/model onto the root's
    // and a `dynamic`'s own node, and onto a model-mode `if`/loop's
    // alongside its own condition/progress keys, so showing it for
    // any other kind would tag a container with the default model
    // whenever one happened to be set, not a model it ever ran.
    let dim = node.and_then(|n| {
        n.meta
            .get("provider")
            .and_then(Value::as_str)
            .or_else(|| {
                matches!(kind, RowKind::Prompt | RowKind::Reflector)
                    .then(|| n.meta.get("model").and_then(Value::as_str))
                    .flatten()
            })
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
            model,
            run_end_ts,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::Event;
    use serde_json::json;

    /// Siblings created in the same second but at different
    /// timestamp precisions (`2026-01-01T00:00:00Z` is earlier than
    /// `...00.200Z`, but sorts *after* it as a raw string, since `.`
    /// (0x2E) sorts below `Z` (0x5A)) must still order by the real
    /// instant, same as `diff::sort_log_lines` already does for the
    /// story/log.
    #[test]
    fn child_segments_sorts_by_real_instant_not_by_string() {
        let state = json!({"prime": {"value": null, "meta": {},
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z"}},
            "b": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00.200Z"}}
        }});
        let flat = flatten_state(&state);
        let mut paths = BTreeMap::new();
        paths.insert("prime.a".to_string(), ());
        paths.insert("prime.b".to_string(), ());
        let plan_order = BTreeMap::new();
        let kids = child_segments("prime", &paths, &flat, &plan_order);
        assert_eq!(
            kids,
            vec![
                ("a".to_string(), "prime.a".to_string()),
                ("b".to_string(), "prime.b".to_string())
            ]
        );
    }

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
        // The document root counts toward "planned" too (issue #620),
        // alongside `step1` itself.
        assert_eq!(state.header.effects_planned, Some(2));
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

    /// Issue #616: both engines write the scope's effective
    /// `adapter`/`model` into a container's own node too, so
    /// `meta.adapter` alone must never make a node with children of
    /// its own read as `Prompt` -- only a leaf, with no children, ever
    /// actually ran one.
    #[test]
    fn from_meta_shape_treats_a_node_with_children_as_a_container_not_a_prompt() {
        let plan = PlanTree::empty();
        let state = json!({"prime": {
            "value": null, "meta": {"completed_at": "t1"},
            "wrap": {
                "value": true,
                "meta": {"completed_at": "t1", "adapter": "_noop", "model": "llama3.1:8b", "flow": "chain"},
                "inner": {"value": "hi\n", "meta": {"completed_at": "t1", "provider": "shell"}}
            }
        }});
        let flat = flatten_state(&state);
        assert_eq!(row_kind_for("prime.wrap", &plan, &flat), RowKind::Unknown);
        assert_ne!(row_kind_for("prime.wrap", &plan, &flat), RowKind::Prompt);
    }

    /// The same shape, minus the child -- a real prompt leaf, with no
    /// children of its own, still reads as one (#616 must not widen
    /// the container rule to swallow actual prompts).
    #[test]
    fn from_meta_shape_still_reads_a_childless_adapter_node_as_a_prompt() {
        let plan = PlanTree::empty();
        let state = json!({"prime": {
            "value": null, "meta": {"completed_at": "t1"},
            "ask": {"value": "the reply", "meta": {"completed_at": "t1", "adapter": "_noop", "model": "llama3.1:8b"}}
        }});
        let flat = flatten_state(&state);
        assert_eq!(row_kind_for("prime.ask", &plan, &flat), RowKind::Prompt);
    }

    /// Issue #616: a model-mode `if` writes `adapter`/`model`
    /// alongside its own `condition_result` as the record of a real
    /// model call -- if it has no child node yet (the else branch
    /// taken with no `else`, or a condition error with `on_error:
    /// skip`), the condition signal must still win over `meta.adapter`
    /// so the row reads `If`, not `Prompt`.
    #[test]
    fn from_meta_shape_reads_a_childless_model_mode_if_as_if_not_prompt() {
        let plan = PlanTree::empty();
        let state = json!({"prime": {
            "value": null, "meta": {"completed_at": "t1"},
            "gate": {
                "value": null,
                "meta": {
                    "completed_at": "t1",
                    "adapter": "_noop",
                    "model": "llama3.1:8b",
                    "condition_result": false,
                    "branch": "else"
                }
            }
        }});
        let flat = flatten_state(&state);
        assert_eq!(row_kind_for("prime.gate", &plan, &flat), RowKind::If);
        assert_ne!(row_kind_for("prime.gate", &plan, &flat), RowKind::Prompt);
    }

    /// Issue #616: with no plan at all (`osi watch`'s own fallback),
    /// the orchestration pane's row dim must show no model for a
    /// container carrying the scope's effective `adapter`/`model`,
    /// while a real (childless) prompt leaf still shows its own.
    #[test]
    fn no_plan_dim_hides_the_model_on_a_container_row_but_keeps_it_on_a_prompt_row() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {
            "value": null, "meta": {"completed_at": "t1", "adapter": "_noop", "model": "llama3.1:8b", "flow": "chain"},
            "wrap": {
                "value": true,
                "meta": {"completed_at": "t1", "adapter": "_noop", "model": "llama3.1:8b", "flow": "chain"},
                "inner": {"value": "hi\n", "meta": {"completed_at": "t1", "provider": "shell"}}
            },
            "ask": {"value": "the reply", "meta": {"completed_at": "t1", "adapter": "_noop", "model": "llama3.1:8b"}}
        }});
        let rendered = build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
                exited_at: None,
            },
            0.0,
            "2026-01-01T00:00:10Z",
            &[],
        );
        let root = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime")
            .expect("root row");
        assert_eq!(root.dim, None);
        let wrap = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.wrap")
            .expect("wrap row");
        assert_eq!(wrap.dim, None);
        let ask = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.ask")
            .expect("ask row");
        assert_eq!(ask.dim.as_deref(), Some("llama3.1:8b"));

        let wrap_state_row = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "wrap")
            .expect("wrap state row");
        assert_eq!(wrap_state_row.dim, None);
        let ask_state_row = rendered
            .state_rows
            .iter()
            .find(|r| r.path == "ask")
            .expect("ask state row");
        assert_eq!(ask_state_row.dim.as_deref(), Some("llama3.1:8b"));
    }

    #[test]
    fn a_named_reports_state_node_is_not_a_row_and_is_not_counted() {
        use electricity_bytecode::{
            EffectPath, Escape, LeafKind, NodeKind, OnError, Op, Region, ReportLevel, ReportOp,
            TemplateText,
        };

        let root_path = EffectPath::root();
        let tool_path = root_path.clone().push_name("step1");
        let report_path = root_path.clone().push_name("status");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![
                        Op {
                            path: tool_path,
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
                        },
                        Op {
                            path: report_path,
                            name: Some("status".to_string()),
                            kind: NodeKind::Leaf(Box::new(LeafKind::Report(ReportOp {
                                message: TemplateText::new("hi", true, Escape::None),
                                data: None,
                                level: ReportLevel::Info,
                                description: None,
                            }))),
                            on_error: OnError::Fail,
                            labels: None,
                            enabled: true,
                        },
                    ],
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
        // A report fires no `effect_start`/`effect_complete`, so it
        // never becomes a plan row (`plan.rs`'s own exclusion). Its
        // state node is still a plain `{value, meta}` node (#605),
        // but #610 keeps it out of the orchestration tree too --
        // `PlanTree::is_report_path` marks a *named* report's own
        // path, which `RunModel::observe` then skips entirely, so
        // there is no row for it at all, not merely an uncounted one.
        let plan = PlanTree::from_program(&program);
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
            "status": {"value": {"message": "hi", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t1", "dry_run": false}}
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

        assert!(
            rendered.rows.iter().all(|r| r.path != "prime.status"),
            "a named report must not be a row in the orchestration tree at all: {:?}",
            rendered.rows
        );

        // `step1` and the (still-running) root count; the report does not.
        assert_eq!(rendered.header.effects_planned, Some(2));
        assert_eq!(rendered.header.effects_done, 1);
    }

    /// #610: a planned tool's own output can coincidentally have the
    /// exact shape a report's state value does (`{message: str,
    /// level: info|warning, data}`) -- `row_kind_for` must still read
    /// it as the tool it really is, from the plan, rather than
    /// guessing `Report` from the value shape alone.
    #[test]
    fn a_tool_whose_output_looks_like_a_report_still_reads_as_a_tool() {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region, ToolOp};

        let root_path = EffectPath::root();
        let tool_path = root_path.clone().push_name("status");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: tool_path,
                        name: Some("status".to_string()),
                        kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                            provider: "json".to_string(),
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
                        }))),
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
        let model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "status": {"value": {"message": "ok", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
        }});
        let details = details_for("prime.status", &plan, &model, Some(&state));
        assert_eq!(details.kind, RowKind::Tool);
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
    fn header_latest_report_is_none_before_any_report_and_then_tracks_the_latest() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(rendered.header.latest_report, None);

        model.observe_event(&Event::Report {
            ts: "t0".to_string(),
            path: "prime".to_string(),
            level: crate::observe::ReportLevel::Info,
            message: "first".to_string(),
            data: None,
            data_truncated: false,
        });
        model.observe_event(&Event::Report {
            ts: "t1".to_string(),
            path: "prime".to_string(),
            level: crate::observe::ReportLevel::Warning,
            message: "second".to_string(),
            data: None,
            data_truncated: false,
        });
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );
        assert_eq!(
            rendered.header.latest_report,
            Some(HeaderReport {
                level: crate::observe::ReportLevel::Warning,
                message: "second".to_string(),
                ts: "t1".to_string(),
            })
        );
    }

    /// #610: a repeated report (the same level and message as the
    /// one before it, e.g. the same "chain pass" message on every
    /// loop pass) must still change `HeaderReport` -- its own `ts`
    /// makes a genuinely new sighting compare unequal to the last
    /// one even when the text is identical, so the header still
    /// highlights (`changes::diff`'s own `header:report` key) on
    /// every repeat, not just the first.
    #[test]
    fn a_repeated_report_still_changes_the_header_report() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        model.observe_event(&Event::Report {
            ts: "t0".to_string(),
            path: "prime".to_string(),
            level: crate::observe::ReportLevel::Info,
            message: "chain pass".to_string(),
            data: None,
            data_truncated: false,
        });
        let first = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            1.0,
            "t0",
            &[],
        );
        model.observe_event(&Event::Report {
            ts: "t1".to_string(),
            path: "prime".to_string(),
            level: crate::observe::ReportLevel::Info,
            message: "chain pass".to_string(),
            data: None,
            data_truncated: false,
        });
        let second = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Running,
            2.0,
            "t1",
            &[],
        );
        assert_ne!(first.header.latest_report, second.header.latest_report);
        let changed = crate::changes::diff(Some(&first), &second);
        assert!(changed.contains(&"header:report".to_string()));
    }

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
            engine: None,
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
                exited_at: None,
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

    /// An aborted run (the engine was killed, so no `run_end` event
    /// and `prime` never got its own `completed_at`) must stop
    /// ticking `elapsed` at the latest real completion anywhere in
    /// state, not keep counting up to `now`.
    #[test]
    fn header_elapsed_stops_at_the_latest_completion_when_a_run_aborts_with_no_run_end() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "2026-01-01T11:04:58Z".to_string(),
            run_id: "r1".to_string(),
            pid: Some(1234),
            engine: None,
        });
        let state_json = json!({
            "prime": {"value": null, "meta": {"created_at": "2026-01-01T11:04:58Z", "completed_at": null},
                "a": {"value": "x", "meta": {"created_at": "2026-01-01T11:04:58Z", "completed_at": "2026-01-01T11:06:00Z"}}
            }
        });
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: true,
                has_failure_reason: false,
                exited_at: None,
            },
            0.0,
            // `now` well after the abort -- elapsed must stop at the
            // latest real completion (11:06:00), not keep ticking.
            "2026-01-01T12:00:00Z",
            &[],
        );
        assert_eq!(
            rendered.header.run_end_ts.as_deref(),
            Some("2026-01-01T11:06:00Z")
        );
        assert_eq!(rendered.header.elapsed_s, 62.0);
    }

    /// A step that was killed, with no `completed_at` of its own,
    /// still counts toward the latest known instant -- a step that
    /// finished earlier must not win over one still running when the
    /// process died.
    #[test]
    fn header_elapsed_counts_a_killed_step_after_an_earlier_completed_one() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({
            "prime": {"value": null, "meta": {"created_at": "2026-01-01T11:00:00Z", "completed_at": null},
                "a": {"value": "x", "meta": {"created_at": "2026-01-01T11:00:00Z", "completed_at": "2026-01-01T11:06:00Z"}},
                "b": {"value": null, "meta": {"created_at": "2026-01-01T11:06:30Z", "completed_at": null}}
            }
        });
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: true,
                has_failure_reason: false,
                exited_at: None,
            },
            0.0,
            "2026-01-01T12:00:00Z",
            &[],
        );
        assert_eq!(
            rendered.header.run_end_ts.as_deref(),
            Some("2026-01-01T11:06:30Z")
        );
        assert_eq!(rendered.header.elapsed_s, 390.0);
    }

    /// A run that's ended with nothing known at all to end `elapsed`
    /// at -- no `run_end` event, no `prime.meta.completed_at`, no
    /// step that ever started or completed, and no process-exit
    /// instant -- freezes `elapsed` at the first `now` it's built
    /// with, rather than counting on forever on every later rebuild.
    #[test]
    fn header_elapsed_freezes_when_a_run_has_no_completed_step_at_all() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state_json = json!({
            "prime": {"value": null, "meta": {"completed_at": null}}
        });
        let process = ProcessState::Exited {
            interrupted: false,
            likely_failed: true,
            has_failure_reason: true,
            exited_at: None,
        };
        let first = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            process.clone(),
            0.0,
            "2026-01-01T11:30:00Z",
            &[],
        );
        assert_eq!(
            first.header.run_end_ts.as_deref(),
            Some("2026-01-01T11:30:00Z")
        );

        let second = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state_json),
            process,
            0.0,
            "2026-01-01T11:45:00Z",
            &[],
        );
        assert_eq!(
            second.header.run_end_ts.as_deref(),
            Some("2026-01-01T11:30:00Z")
        );
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
        // The pass label is the details pane's own
        // *title*, not its `plan` line.
        let details = details_for(
            "prime.each_tree.iter_2",
            &plan,
            &RunModel::new(),
            Some(&state_json),
        );
        assert_eq!(details.title.as_deref(), Some("pass 3 of 8 (iter_2)"));
        assert_eq!(details.plan_summary, "");
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

    /// A report fires only `Store::fire_report`, never effect_start/
    /// effect_complete, so adding one at the top level and one inside
    /// a running loop's body must not change `effects_planned`/
    /// `effects_done` at all -- a finished run must not show a
    /// pending report forever.
    #[test]
    fn reports_do_not_change_effects_planned_or_effects_done() {
        fn with_a_report_added(
            mut program: electricity_bytecode::Program,
        ) -> electricity_bytecode::Program {
            use electricity_bytecode::{
                EffectPath, Escape, LeafKind, LoopId, NodeKind, OnError, Op, Region, ReportLevel,
                ReportOp, TemplateText,
            };

            fn report_op(path: EffectPath) -> Op {
                Op {
                    path,
                    name: None,
                    kind: NodeKind::Leaf(Box::new(LeafKind::Report(ReportOp {
                        message: TemplateText::new("status", true, Escape::None),
                        data: None,
                        level: ReportLevel::Info,
                        description: None,
                    }))),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                }
            }

            if let NodeKind::Control(Region::Block { ops, .. }) = &mut program.root.kind {
                ops.insert(0, report_op(program.root.path.clone()));
                if let Some(Op {
                    kind: NodeKind::Control(Region::Loop { body, .. }),
                    path: loop_path,
                    ..
                }) = ops.get_mut(1)
                {
                    if let Region::Block { ops: body_ops, .. } = body.as_mut() {
                        body_ops.insert(0, report_op(loop_path.clone().push_pass(LoopId(0))));
                    }
                }
            }
            program
        }

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 2, "total": 4}},
                "iter_0": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                "iter_1": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
            }
        }});

        let plan_without = PlanTree::from_program(&named_loop_with_one_leaf_program());
        let plan_with =
            PlanTree::from_program(&with_a_report_added(named_loop_with_one_leaf_program()));

        let mut model = RunModel::new();
        let rendered_without = build(
            "do-thing.yml",
            "cof",
            &plan_without,
            &mut model,
            Some(&state),
            ProcessState::Running,
            2.0,
            "t2",
            &[],
        );
        let mut model = RunModel::new();
        let rendered_with = build(
            "do-thing.yml",
            "cof",
            &plan_with,
            &mut model,
            Some(&state),
            ProcessState::Running,
            2.0,
            "t2",
            &[],
        );

        assert_eq!(
            rendered_with.header.effects_planned,
            rendered_without.header.effects_planned
        );
        assert_eq!(
            rendered_with.header.effects_done,
            rendered_without.header.effects_done
        );
        // The root and `fan`'s own container both count too (issue
        // #620), alongside the body leaf multiplied by its known total.
        assert_eq!(rendered_without.header.effects_planned, Some(6));
        assert_eq!(rendered_without.header.effects_done, 2);
    }

    /// #610's own acceptance line: "a loop pass holding a named
    /// report plus one step folds exactly like a pass without the
    /// report" -- `fold_rows`' breadcrumb fold (`fold.rs`) only
    /// applies to a container with exactly *one* child, so a named
    /// report that wrongly became a second row under the pass would
    /// silently turn this test's own single-step breadcrumb back into
    /// two separate, unfolded rows.
    #[test]
    fn a_loop_pass_holding_a_named_report_plus_one_step_folds_the_same_as_without_it() {
        fn with_a_named_report_added(
            mut program: electricity_bytecode::Program,
        ) -> electricity_bytecode::Program {
            use electricity_bytecode::{
                Escape, LeafKind, LoopId, NodeKind, OnError, Op, Region, ReportLevel, ReportOp,
                TemplateText,
            };
            if let NodeKind::Control(Region::Block { ops, .. }) = &mut program.root.kind {
                if let Some(Op {
                    kind: NodeKind::Control(Region::Loop { body, .. }),
                    path: loop_path,
                    ..
                }) = ops.get_mut(0)
                {
                    if let Region::Block { ops: body_ops, .. } = body.as_mut() {
                        let pass_path = loop_path.clone().push_pass(LoopId(0));
                        body_ops.insert(
                            0,
                            Op {
                                path: pass_path.push_name("status"),
                                name: Some("status".to_string()),
                                kind: NodeKind::Leaf(Box::new(LeafKind::Report(ReportOp {
                                    message: TemplateText::new("status", true, Escape::None),
                                    data: None,
                                    level: ReportLevel::Info,
                                    description: None,
                                }))),
                                on_error: OnError::Fail,
                                labels: None,
                                enabled: true,
                            },
                        );
                    }
                }
            }
            program
        }

        fn rows_for(plan: &PlanTree, state: &Value) -> Vec<Row> {
            let mut model = RunModel::new();
            build_folded(
                "do-thing.yml",
                "cof",
                plan,
                &mut model,
                Some(state),
                ProcessState::Running,
                1.0,
                "t1",
                &[],
                true,
                &std::collections::BTreeSet::new(),
            )
            .rows
        }

        let plan_without = PlanTree::from_program(&named_loop_with_one_leaf_program());
        let plan_with = PlanTree::from_program(&with_a_named_report_added(
            named_loop_with_one_leaf_program(),
        ));

        let state_without = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 1}},
                "iter_0": {"nap": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}}
            }
        }});
        // The named report's own state node (#605): a `{value, meta}`
        // node at `prime.fan.iter_0.status`, same shape
        // `exec::report`'s named branch writes.
        let state_with = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 1}},
                "iter_0": {
                    "nap": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                    "status": {"value": {"message": "status", "level": "info", "data": null},
                        "meta": {"created_at": "t0", "completed_at": "t0"}}
                }
            }
        }});

        let rows_without = rows_for(&plan_without, &state_without);
        let rows_with = rows_for(&plan_with, &state_with);

        let labels_without: Vec<_> = rows_without.iter().map(|r| r.label.as_str()).collect();
        let labels_with: Vec<_> = rows_with.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels_with, labels_without,
            "a named report in the pass must not change which rows fold or how"
        );
        assert!(
            labels_without.iter().any(|l| l.contains('›')),
            "the single-step pass should still fold into a breadcrumb: {labels_without:?}"
        );
        assert!(
            rows_with.iter().all(|r| !r.path.ends_with("status")),
            "the named report must never be a row in the orchestration tree: {rows_with:?}"
        );
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
        // The root and `fan`'s own container both count too (issue
        // #620): 2 (root, fan) + 4 (the body leaf, known total) = 6.
        assert_eq!(rendered.header.effects_planned, Some(6));
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
        // The root and `fan`'s own container both count (issue
        // #620) alongside the body leaf's own lower bound: 2 + 1.
        assert_eq!(rendered.header.effects_planned, Some(3));
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
        // The root counts too (issue #620); the unnamed loop itself
        // never gets a node of its own, so only the root and the
        // body leaf's own lower bound contribute: 1 + 1.
        assert_eq!(rendered.header.effects_planned, Some(2));
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
        // The root counts too (issue #620), alongside `my_use`'s own
        // fallback count (no child discovered yet): 1 + 1.
        assert_eq!(rendered.header.effects_planned, Some(2));
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
        // The root counts too (issue #620), alongside the one
        // discovered child: 1 + 1.
        assert_eq!(
            rendered.header.effects_planned,
            Some(2),
            "one discovered child so far, plus the root, not the use node too"
        );
        assert!(
            rendered.header.effects_planned_is_lower_bound,
            "my_use is still open -- more children could still appear"
        );
        assert_eq!(rendered.header.effects_done, 1);
        assert!(rendered.header.effects_done <= rendered.header.effects_planned.unwrap());

        // Now both children are done, `my_use` itself has finished,
        // and the run has ended -- planned becomes exact, matching
        // done exactly: the root, `child1` and `child2` (issue #620;
        // `my_use`'s own node still doesn't double-count once its
        // children have been discovered).
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
        assert_eq!(rendered.header.effects_planned, Some(3));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 3);
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
        // The root counts too (issue #620), alongside `my_use`'s own
        // fallback node: 1 + 1.
        assert_eq!(rendered.header.effects_planned, Some(2));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 2);
    }

    /// A declared-outputs `use`: its children ran (the engine's own
    /// `effects_run` counts them) but never land in the final state
    /// at all, so a walk over state alone can only ever see the root
    /// and the `use` node's own fallback entry -- 2, not the engine's
    /// real 4 (root, `my_use`, `child1`, `child2`). Once the run has
    /// ended, the header must read the engine's own total instead of
    /// that undercount.
    #[test]
    fn effects_planned_and_done_read_the_engines_own_total_for_a_declared_outputs_use() {
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
        // No `child1`/`child2` under `my_use` at all -- a declared-
        // outputs `use` drops its children from the final state even
        // once it succeeds (DESIGN.md §1.2) -- but the engine's own
        // `effects_run` still counted all four.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1", "totals": {"effects_run": 4}}},
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
        assert_eq!(rendered.header.effects_planned, Some(4));
        assert!(!rendered.header.effects_planned_is_lower_bound);
        assert_eq!(rendered.header.effects_done, 4);
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
        // The root counts too (issue #620), alongside the two
        // grafted children.
        assert_eq!(rendered.header.effects_planned, Some(3));
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
        assert_eq!(rendered.header.effects_planned, Some(3));
        assert_eq!(rendered.header.effects_done, 3);
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
        let details = details_for("prime.tool", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.ask", &plan, &RunModel::new(), Some(&state_json));
        assert!(details.prompt_sent.as_ref().unwrap().len() < long_prompt.len());
        let full = full_value("prime.ask", FullValueField::PromptSent, Some(&state_json));
        assert_eq!(full, Some(long_prompt));
    }

    /// #610: a named report's details show its type, level, the
    /// latest message and clock time, and `data` as pretty JSON --
    /// falling back to the state node's own copy when no `--events`
    /// sighting for the path exists at all.
    #[test]
    fn details_for_a_named_report_falls_back_to_the_state_node_with_no_events() {
        let plan = PlanTree::empty();
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "status": {"value": {"message": "on track", "level": "info", "data": {"n": 3}},
                "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let details = details_for("prime.status", &plan, &RunModel::new(), Some(&state_json));
        assert_eq!(details.kind, RowKind::Report);
        assert!(
            details
                .meta_summary
                .contains(&("level".to_string(), "info".to_string()))
        );
        assert!(
            details
                .meta_summary
                .contains(&("message".to_string(), "on track".to_string()))
        );
        assert!(
            details
                .meta_summary
                .iter()
                .any(|(k, v)| k == "time" && v == "t1")
        );
        let value = details.value.expect("a report with data gets a value");
        assert!(value.contains("\"n\": 3"), "{value}");
    }

    /// The latest `--events` sighting for the path wins over the
    /// state node's own (possibly stale) copy.
    #[test]
    fn details_for_a_named_report_prefers_the_latest_event_sighting() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        model.observe_event(&Event::Report {
            ts: "t2".to_string(),
            path: "prime.status".to_string(),
            level: crate::observe::ReportLevel::Warning,
            message: "falling behind".to_string(),
            data: Some(json!({"n": 9})),
            data_truncated: false,
        });
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "status": {"value": {"message": "on track", "level": "info", "data": {"n": 3}},
                "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let details = details_for("prime.status", &plan, &model, Some(&state_json));
        assert!(
            details
                .meta_summary
                .contains(&("level".to_string(), "warning".to_string()))
        );
        assert!(
            details
                .meta_summary
                .contains(&("message".to_string(), "falling behind".to_string()))
        );
        let value = details.value.expect("a report with data gets a value");
        assert!(value.contains("\"n\": 9"), "{value}");
    }

    /// #610: the root row's own details carry `run_id`, now that
    /// the header shows a report there instead once one arrives.
    #[test]
    fn details_for_the_root_row_shows_the_run_id() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "t0".to_string(),
            run_id: "run-xyz".to_string(),
            pid: Some(1),
            engine: None,
        });
        let state_json = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let details = details_for("prime", &plan, &model, Some(&state_json));
        assert!(
            details
                .meta_summary
                .contains(&("run_id".to_string(), "run-xyz".to_string()))
        );
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
        let details = details_for("prime.ask", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.ask", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.ask", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.gate", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.ask", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.gate", &plan, &RunModel::new(), Some(&state_json));
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
        let details = details_for("prime.ls", &plan, &RunModel::new(), Some(&state_json));
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
                    "ask": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z", "tokens_sent": 5, "tokens_received": 7, "adapter": "scripted", "model": "m"}}
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
    /// own. Only `cof` can ever produce a running agent -- electricity
    /// refuses the `agent` provider outright (DESIGN.md) -- but the
    /// state shape a running tool node writes is otherwise identical
    /// on both engines, so this one fixture covers the shape either
    /// would write.
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

    /// Issue #637: a node with a fresh `meta.live` heartbeat counts
    /// as an agent in flight purely from `flat`, with no `provider`
    /// field and no `--events` confirmation of its own at all.
    #[test]
    fn a_fresh_meta_live_heartbeat_counts_as_an_agent_in_flight_with_no_provider_or_events() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "agent1": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                "live": {"updated_at": "2026-10-08T11:52:54Z"}}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-10-08T11:52:55Z",
            &[],
        );
        assert_eq!(rendered.token_bar.agents_running, 1);
    }

    /// An interrupted run's own leftover `meta.live` (DESIGN.md) must
    /// never count as an agent in flight once its own heartbeat has
    /// gone stale, even though the node itself still has no
    /// `completed_at`.
    #[test]
    fn a_stale_meta_live_heartbeat_does_not_count_as_an_agent_in_flight() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "agent1": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                "live": {"updated_at": "2026-10-08T11:46:50Z"}}}
        }});
        let rendered = build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-10-08T11:52:55Z",
            &[],
        );
        assert_eq!(rendered.token_bar.agents_running, 0);
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

    /// A tree-loop pass's tool child, same as its prompt counterpart
    /// above, has no node in `flat` at all while `cof` runs it -- the
    /// token bar's "agents in flight" count must still find an
    /// `agent` leaf's own `provider` from the plan.
    fn named_loop_with_one_tool_program(provider: &str) -> electricity_bytecode::Program {
        named_loop_with_tool_children_program(&[("work", provider)])
    }

    /// `named_loop_with_one_tool_program`, generalised to any number
    /// of tool children in the loop's own body -- two or more, to
    /// check quick-steps folding for rows known only from `--events`.
    fn named_loop_with_tool_children_program(
        children: &[(&str, &str)],
    ) -> electricity_bytecode::Program {
        use electricity_bytecode::EffectPath;
        use electricity_bytecode::{
            LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode, Program,
            Region, ToolOp,
        };

        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let ops = children
            .iter()
            .map(|(name, provider)| Op {
                path: pass_path.clone().push_name(*name),
                name: Some(name.to_string()),
                kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                    provider: provider.to_string(),
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
            })
            .collect();
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
                            body: Box::new(Region::Block { ops, overlay: true }),
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

    /// A running agent known only from `--events` (no state node at
    /// all, as `cof` writes a tree-loop pass) is still counted in
    /// flight, from the plan's own `ToolOp.provider`.
    #[test]
    fn a_running_agent_known_only_from_events_is_still_counted_in_flight() {
        let plan = PlanTree::from_program(&named_loop_with_one_tool_program("agent"));
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.work".to_string(),
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
        assert_eq!(rendered.token_bar.agents_running, 1);
        assert_eq!(rendered.token_bar.prompts_running, 0);
        assert_eq!(rendered.token_bar.estimated_input, 0);
    }

    /// A running tool known only from events, whose plan `provider`
    /// isn't `agent`, is counted as neither a prompt nor an agent.
    #[test]
    fn a_running_non_agent_tool_known_only_from_events_is_not_counted_in_flight() {
        let plan = PlanTree::from_program(&named_loop_with_one_tool_program("shell"));
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.work".to_string(),
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
        assert_eq!(rendered.token_bar.agents_running, 0);
        assert_eq!(rendered.token_bar.prompts_running, 0);
    }

    /// A row with no state node at all (a tree-loop pass's tool
    /// child) still shows its own elapsed time, from `RunModel`'s
    /// own record of the matching `start` event's `ts` -- `now`, not
    /// `node.created_at`, since there is no node.
    #[test]
    fn a_running_row_known_only_from_events_shows_its_own_elapsed_time() {
        let plan = PlanTree::from_program(&named_loop_with_one_tool_program("shell"));
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "2026-01-01T11:00:00Z".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.work".to_string(),
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
            "2026-01-01T11:00:05Z",
            &[],
        );
        let row = rendered
            .rows
            .iter()
            .find(|r| r.path == "prime.fan.iter_0.work")
            .expect("row should exist");
        assert_eq!(row.duration_s, Some(5.0));
    }

    /// Two or more finished tool steps known only
    /// from `--events` -- no state node for either -- still carry a
    /// real duration (from `RunModel`'s own record of each one's
    /// matching `end` event `ms`), so they fold into one quick-steps
    /// row by the same rule as any other row.
    #[test]
    fn finished_rows_known_only_from_events_fold_into_quick_steps() {
        let plan = PlanTree::from_program(&named_loop_with_tool_children_program(&[
            ("q1", "shell"),
            ("q2", "shell"),
        ]));
        let mut model = RunModel::new();
        for (name, ms) in [("q1", 300u64), ("q2", 200u64)] {
            let path = format!("prime.fan.iter_0.{name}");
            model.observe_event(&Event::Start {
                ts: "2026-01-01T11:00:00Z".to_string(),
                id: Some(1),
                path: path.clone(),
            });
            model.observe_event(&Event::End {
                ts: "2026-01-01T11:00:01Z".to_string(),
                id: Some(1),
                path,
                ok: true,
                ms: Some(ms),
                error: None,
            });
        }
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rendered = build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T11:00:05Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        let summary = rendered
            .rows
            .iter()
            .find(|r| r.kind == RowKind::QuickSteps)
            .expect("the two finished event-only steps should fold into one summary row");
        assert_eq!(summary.label, "2 quick steps");
        assert!((summary.duration_s.unwrap() - 0.5).abs() < 1e-9);
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

        // Issue #613: folding runs on every rebuild too (at most once
        // a second) -- a single depth-first pass over `rendered.rows`
        // (build the tree, fold, flatten back), never anything
        // quadratic in the row count.
        let fold_start = std::time::Instant::now();
        let folded = crate::fold::fold_rows(
            rendered.rows.clone(),
            true,
            &std::collections::BTreeSet::new(),
        );
        let fold_elapsed = fold_start.elapsed();
        // This fixture's own timestamps (`t0`, `t1`, ...) aren't real
        // RFC-3339, so none of these rows has a known duration to
        // fold by at all -- the row count is unchanged either way;
        // what this pins is that folding 2000 rows is still fast.
        assert_eq!(folded.len(), rendered.rows.len());
        assert!(
            fold_elapsed < std::time::Duration::from_millis(500),
            "folding a 2000-row tree took {fold_elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );

        // `build_folded` itself -- `build`
        // plus folding, the path the TUI actually calls every redraw
        // -- must stay linear too, not just `fold_rows` in isolation.
        let build_folded_start = std::time::Instant::now();
        let built_and_folded = build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            10.0,
            "t2001",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        let build_folded_elapsed = build_folded_start.elapsed();
        assert_eq!(built_and_folded.rows.len(), rendered.rows.len());
        assert!(
            build_folded_elapsed < std::time::Duration::from_millis(500),
            "build_folded on a 2000-row tree took {build_folded_elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );
    }

    // Issue #620: once a run has deterministically ended, "effects
    // done out of planned" must read N/N, matching `runtime.last_run.
    // totals.effects_run` -- every shape below hand-builds the IR for
    // one document and the *final* state a real engine would have
    // written for it, then checks that `effects_planned` equals
    // `effects_done` (and both equal the node count a real run would
    // report) with `ProcessState::exited(false)`, never interrupted.

    fn tool_leaf(path: electricity_bytecode::EffectPath, name: &str) -> electricity_bytecode::Op {
        use electricity_bytecode::{LeafKind, NodeKind, OnError, Op, ParamNode, ToolOp};
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

    fn disabled_tool_leaf(
        path: electricity_bytecode::EffectPath,
        name: &str,
    ) -> electricity_bytecode::Op {
        electricity_bytecode::Op {
            enabled: false,
            ..tool_leaf(path, name)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn if_op(
        path: electricity_bytecode::EffectPath,
        name: Option<&str>,
        on_error: electricity_bytecode::OnError,
        enabled: bool,
        then_ops: Vec<electricity_bytecode::Op>,
        else_ops: Option<Vec<electricity_bytecode::Op>>,
    ) -> electricity_bytecode::Op {
        use electricity_bytecode::{Condition, NodeKind, Op, Region};
        Op {
            path,
            name: name.map(str::to_string),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                },
                then_: Box::new(Region::Block {
                    ops: then_ops,
                    overlay: true,
                }),
                else_: else_ops.map(|ops| Box::new(Region::Block { ops, overlay: true })),
                threshold: 0.5,
            }),
            on_error,
            labels: None,
            enabled,
        }
    }

    fn dynamic_chain_op(
        path: electricity_bytecode::EffectPath,
        name: Option<&str>,
        on_error: electricity_bytecode::OnError,
        ops: Vec<electricity_bytecode::Op>,
    ) -> electricity_bytecode::Op {
        use electricity_bytecode::{NodeKind, Op, Region};
        Op {
            path,
            name: name.map(str::to_string),
            kind: NodeKind::Control(Region::Block {
                ops,
                overlay: false,
            }),
            on_error,
            labels: None,
            enabled: true,
        }
    }

    fn each_loop_op(
        path: electricity_bytecode::EffectPath,
        name: Option<&str>,
        flow: electricity_bytecode::LoopFlow,
        in_path: &str,
        body_ops: Vec<electricity_bytecode::Op>,
    ) -> electricity_bytecode::Op {
        use electricity_bytecode::{LoopSpec, NodeKind, OnError, Op, Region};
        Op {
            path,
            name: name.map(str::to_string),
            kind: NodeKind::Control(Region::Loop {
                name: name.map(str::to_string),
                spec: LoopSpec::Each {
                    in_path: in_path.to_string(),
                    as_name: "item".to_string(),
                    truncate: false,
                },
                body: Box::new(Region::Block {
                    ops: body_ops,
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
        }
    }

    fn while_loop_op(
        path: electricity_bytecode::EffectPath,
        name: Option<&str>,
        max_iterations: Option<u32>,
        body_ops: Vec<electricity_bytecode::Op>,
    ) -> electricity_bytecode::Op {
        use electricity_bytecode::{Condition, LoopFlow, LoopSpec, NodeKind, OnError, Op, Region};
        Op {
            path,
            name: name.map(str::to_string),
            kind: NodeKind::Control(Region::Loop {
                name: name.map(str::to_string),
                spec: LoopSpec::While(Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                }),
                body: Box::new(Region::Block {
                    ops: body_ops,
                    overlay: true,
                }),
                flow: LoopFlow::Chain,
                max_concurrency: None,
                max_iterations,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn wrap_root(ops: Vec<electricity_bytecode::Op>) -> electricity_bytecode::Program {
        use electricity_bytecode::{EffectPath, NodeKind, OnError, Op, Program, Region};
        Program {
            root: Op {
                path: EffectPath::root(),
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops,
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

    /// A finished run's header must read `effects_planned ==
    /// effects_done`, and both must equal `expected` -- the node
    /// count a real engine run would report in `effects_run` for this
    /// shape (DESIGN.md §1.2: every completed node, root and named
    /// containers included).
    fn assert_finished_n_of_n(plan: &PlanTree, state: &Value, expected: u64) {
        let mut model = RunModel::new();
        let rendered = build(
            "do.yml",
            "cof",
            plan,
            &mut model,
            Some(state),
            ProcessState::exited(false),
            1.0,
            "t1",
            &[],
        );
        assert_eq!(
            rendered.header.effects_planned,
            Some(expected),
            "effects_planned"
        );
        assert_eq!(rendered.header.effects_done, expected, "effects_done");
        assert!(!rendered.header.effects_planned_is_lower_bound);
    }

    #[test]
    fn if_untaken_branch_with_no_else_leaves_the_planned_total_once_finished() {
        use electricity_bytecode::EffectPath;
        let untaken_path = EffectPath::root().push_name("untaken");
        let after_path = EffectPath::root().push_name("after");
        let program = wrap_root(vec![
            if_op(
                EffectPath::root(),
                None,
                electricity_bytecode::OnError::Fail,
                true,
                vec![tool_leaf(untaken_path, "untaken")],
                None,
            ),
            tool_leaf(after_path, "after"),
        ]);
        let plan = PlanTree::from_program(&program);
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "after": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            }
        });
        // The root and `after` -- `untaken`'s own branch never ran and
        // never will.
        assert_finished_n_of_n(&plan, &state, 2);
    }

    #[test]
    fn if_branches_with_differing_names_leave_the_untaken_one_out_once_finished() {
        use electricity_bytecode::EffectPath;
        let then_path = EffectPath::root().push_name("then_child");
        let else_path = EffectPath::root().push_name("else_child");
        let program = wrap_root(vec![if_op(
            EffectPath::root(),
            None,
            electricity_bytecode::OnError::Fail,
            true,
            vec![tool_leaf(then_path, "then_child")],
            Some(vec![tool_leaf(else_path, "else_child")]),
        )]);
        let plan = PlanTree::from_program(&program);
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "else_child": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            }
        });
        // The root and `else_child` -- `then_child` has a path of its
        // own (the two branches don't share a name, DESIGN.md's own
        // recommendation notwithstanding) and never ran.
        assert_finished_n_of_n(&plan, &state, 2);
    }

    #[test]
    fn if_branches_sharing_a_name_count_once_either_way_once_finished() {
        use electricity_bytecode::EffectPath;
        let then_path = EffectPath::root().push_name("chosen");
        let else_path = EffectPath::root().push_name("chosen");
        let program = wrap_root(vec![if_op(
            EffectPath::root(),
            None,
            electricity_bytecode::OnError::Fail,
            true,
            vec![tool_leaf(then_path, "chosen")],
            Some(vec![tool_leaf(else_path, "chosen")]),
        )]);
        let plan = PlanTree::from_program(&program);
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "chosen": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            }
        });
        assert_finished_n_of_n(&plan, &state, 2);
    }

    /// A named `if` nested inside a named loop's own body, taking a
    /// different branch on different passes -- the issue's own
    /// "different branches taken in different passes" shape, chain
    /// flow.
    #[test]
    fn an_if_inside_a_chain_loop_pass_with_different_branches_each_pass_matches_once_finished() {
        run_if_inside_a_loop_pass_test(electricity_bytecode::LoopFlow::Chain);
    }

    /// [`an_if_inside_a_chain_loop_pass_with_different_branches_each_pass_matches_once_finished`],
    /// tree flow -- the final state shape (and so the count) is
    /// identical; only the loop's own `flow` differs.
    #[test]
    fn an_if_inside_a_tree_loop_pass_with_different_branches_each_pass_matches_once_finished() {
        run_if_inside_a_loop_pass_test(electricity_bytecode::LoopFlow::Tree);
    }

    fn run_if_inside_a_loop_pass_test(flow: electricity_bytecode::LoopFlow) {
        use electricity_bytecode::{EffectPath, LoopId, OnError};
        let loop_path = EffectPath::root().push_name("lp");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let gate_path = pass_path.push_name("gate");
        let then_path = gate_path.clone().push_name("chosen");
        let else_path = gate_path.clone().push_name("chosen");
        let gate = if_op(
            gate_path,
            Some("gate"),
            OnError::Fail,
            true,
            vec![tool_leaf(then_path, "chosen")],
            Some(vec![tool_leaf(else_path, "chosen")]),
        );
        let program = wrap_root(vec![each_loop_op(
            loop_path,
            Some("lp"),
            flow,
            "prime.items.value",
            vec![gate],
        )]);
        let plan = PlanTree::from_program(&program);
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "lp": {"value": null, "meta": {"completed_at": "t1", "mode": "each", "progress": {"done": 3, "total": 3}},
                    "iter_0": {"gate": {"value": {"result": true, "branch": "then", "effects": []},
                        "meta": {"completed_at": "t1", "mode": "cel", "branch": "then"},
                        "chosen": {"value": "a", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}}},
                    "iter_1": {"gate": {"value": {"result": false, "branch": "else", "effects": []},
                        "meta": {"completed_at": "t1", "mode": "cel", "branch": "else"},
                        "chosen": {"value": "b", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}}},
                    "iter_2": {"gate": {"value": {"result": true, "branch": "then", "effects": []},
                        "meta": {"completed_at": "t1", "mode": "cel", "branch": "then"},
                        "chosen": {"value": "a", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}}}
                }
            }
        });
        // The root, `lp` itself, and each of the 3 passes' own `gate`
        // plus its taken `chosen` leaf: 1 + 1 + 3 * 2.
        assert_finished_n_of_n(&plan, &state, 8);
    }

    #[test]
    fn each_over_an_empty_list_leaves_the_body_out_of_the_planned_total_once_finished() {
        use electricity_bytecode::{EffectPath, LoopFlow, LoopId};
        let loop_path = EffectPath::root().push_name("lp");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let body_path = pass_path.push_name("never_runs");
        let program = wrap_root(vec![each_loop_op(
            loop_path,
            Some("lp"),
            LoopFlow::Chain,
            "prime.items.value",
            vec![tool_leaf(body_path, "never_runs")],
        )]);
        let plan = PlanTree::from_program(&program);
        // A zero-pass loop's own node never even gets a `progress`
        // key at all (measured against a real `cof` run): no
        // `iter_*` children either way.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "lp": {"value": null, "meta": {"completed_at": "t1", "mode": "each"}}
            }
        });
        // The root and `lp` itself -- its body never ran at all.
        assert_finished_n_of_n(&plan, &state, 2);
    }

    #[test]
    fn a_while_loop_that_stops_before_max_iterations_matches_once_finished() {
        use electricity_bytecode::{EffectPath, LoopId};
        let loop_path = EffectPath::root().push_name("wl");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let body_path = pass_path.push_name("tick");
        let program = wrap_root(vec![while_loop_op(
            loop_path,
            Some("wl"),
            Some(10),
            vec![tool_leaf(body_path, "tick")],
        )]);
        let plan = PlanTree::from_program(&program);
        // `progress.total` for a `while` loop is its own
        // `max_iterations` (10), never the number of passes it
        // actually ran (3) -- DESIGN.md's own "loop" reference entry.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "wl": {"value": null, "meta": {"completed_at": "t1", "mode": "cel", "progress": {"done": 3, "total": 10}},
                    "iter_0": {"tick": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}},
                    "iter_1": {"tick": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}},
                    "iter_2": {"tick": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}}
                }
            }
        });
        // The root, `wl` itself, and its 3 actual passes -- never the
        // stale `max_iterations` upper bound.
        assert_finished_n_of_n(&plan, &state, 5);
    }

    #[test]
    fn an_if_decided_null_by_on_error_skip_leaves_both_branches_out_once_finished() {
        use electricity_bytecode::{EffectPath, OnError};
        let gate_path = EffectPath::root().push_name("gate");
        let then_path = gate_path.clone().push_name("chosen_then");
        let else_path = gate_path.clone().push_name("chosen_else");
        let after_path = EffectPath::root().push_name("after");
        let program = wrap_root(vec![
            if_op(
                gate_path,
                Some("gate"),
                OnError::Skip,
                true,
                vec![tool_leaf(then_path, "chosen_then")],
                Some(vec![tool_leaf(else_path, "chosen_else")]),
            ),
            tool_leaf(after_path, "after"),
        ]);
        let plan = PlanTree::from_program(&program);
        // The condition itself raised; `on_error: skip` swallows it
        // and runs *neither* branch (DESIGN.md's own guidebook entry
        // on `if`'s own `skip`/`continue` split) -- `gate`'s own node
        // still gets written, with `branch: null` and no `chosen_*`
        // child ever appearing.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "gate": {"value": {"result": null, "branch": null, "effects": {}},
                    "meta": {"completed_at": "t1", "mode": "cel", "error": "CEL evaluation failed"}},
                "after": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            }
        });
        // The root, `gate` itself (failed, handled), and `after` --
        // neither `chosen_then` nor `chosen_else` ever ran.
        assert_finished_n_of_n(&plan, &state, 3);
    }

    #[test]
    fn a_chain_dynamics_on_error_skip_leaves_its_stopped_siblings_out_once_finished() {
        use electricity_bytecode::{EffectPath, OnError};
        let task_path = EffectPath::root().push_name("task");
        let step_a_path = task_path.clone().push_name("step_a");
        let step_b_path = task_path.clone().push_name("step_b");
        let after_path = EffectPath::root().push_name("after");
        let program = wrap_root(vec![
            dynamic_chain_op(
                task_path,
                Some("task"),
                OnError::Skip,
                vec![
                    tool_leaf(step_a_path, "step_a"),
                    tool_leaf(step_b_path, "step_b"),
                ],
            ),
            tool_leaf(after_path, "after"),
        ]);
        let plan = PlanTree::from_program(&program);
        // `step_a` fails under its own default `on_error: fail`,
        // which stops the chain before `step_b` ever starts; `task`'s
        // own `on_error: skip` then swallows that failure onto its
        // own `meta.error`, letting `after` run (DESIGN.md's dynamic
        // reference entry on `on_error`).
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "task": {"value": null, "meta": {"completed_at": "t1", "error": "step_a failed"},
                    "step_a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
                },
                "after": {"value": "", "meta": {"created_at": "t1", "completed_at": "t1", "provider": "shell"}}
            }
        });
        // The root, `task` itself, `task.step_a`, and `after` -- never
        // `task.step_b`.
        assert_finished_n_of_n(&plan, &state, 4);
    }

    #[test]
    fn a_disabled_leaf_still_counts_once_finished() {
        use electricity_bytecode::EffectPath;
        let a_path = EffectPath::root().push_name("leaf_a");
        let b_path = EffectPath::root().push_name("leaf_b");
        let program = wrap_root(vec![
            tool_leaf(a_path, "leaf_a"),
            disabled_tool_leaf(b_path, "leaf_b"),
        ]);
        let plan = PlanTree::from_program(&program);
        // A disabled leaf still fires its own start/end hooks and
        // writes a skip node (DESIGN.md's own "Disabling Effects"
        // reference entry) -- it still counts.
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "leaf_a": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                "leaf_b": {"value": null, "meta": {"created_at": "t1", "completed_at": "t1", "disabled": true}}
            }
        });
        assert_finished_n_of_n(&plan, &state, 3);
    }

    #[test]
    fn a_disabled_containers_children_never_count_once_finished() {
        use electricity_bytecode::{EffectPath, OnError};
        let step1_path = EffectPath::root().push_name("step1");
        let gate_path = EffectPath::root().push_name("gate");
        let chosen_path = gate_path.clone().push_name("chosen");
        let step2_path = EffectPath::root().push_name("step2");
        let program = wrap_root(vec![
            tool_leaf(step1_path, "step1"),
            if_op(
                gate_path,
                Some("gate"),
                OnError::Fail,
                false,
                vec![disabled_tool_leaf(chosen_path, "chosen")],
                None,
            ),
            tool_leaf(step2_path, "step2"),
        ]);
        let plan = PlanTree::from_program(&program);
        // A disabled container's own node is written, but nothing
        // inside it ever runs at all -- no `chosen` node, ever
        // (DESIGN.md's own "Disabling Effects" reference entry).
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "step1": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
                "gate": {"value": {"result": null, "branch": null, "effects": {}},
                    "meta": {"created_at": "t1", "completed_at": "t1", "disabled": true}},
                "step2": {"value": "", "meta": {"created_at": "t1", "completed_at": "t1", "provider": "shell"}}
            }
        });
        // The root, `step1`, `gate` itself, and `step2` -- never `gate.chosen`.
        assert_finished_n_of_n(&plan, &state, 4);
    }
}
