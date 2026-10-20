//! `RunModel`: the rows `osi` renders, their status (DESIGN.md §2),
//! loop progress, run totals, and `$ref`/`last` alias handling. O-1
//! work.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::observe::Event;
use crate::plan::{Flow, PlanTree};

/// A tool name's own `{calls, ms}` entry under `meta.live.tools`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentLiveToolUsage {
    pub calls: u64,
    pub ms: u64,
}

impl AgentLiveToolUsage {
    /// Parses a `{tool_name: {calls, ms}}` map, the shape both
    /// `meta.live.tools` and a finished agent's own `meta.raw.
    /// tool_counts` share (`docs/plugins/agent.md`) -- a missing or
    /// wrong-typed field in one entry just leaves it `0`, same
    /// permissive rule [`AgentLive::from_value`] follows throughout.
    pub fn map_from_value(v: &Value) -> BTreeMap<String, AgentLiveToolUsage> {
        let mut tools = BTreeMap::new();
        if let Some(map) = v.as_object() {
            for (name, entry) in map {
                let calls = entry.get("calls").and_then(Value::as_u64).unwrap_or(0);
                let ms = entry.get("ms").and_then(Value::as_u64).unwrap_or(0);
                tools.insert(name.clone(), AgentLiveToolUsage { calls, ms });
            }
        }
        tools
    }
}

/// `meta.live.current_tool` while a tool runs, else `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLiveCurrentTool {
    pub name: String,
    pub started_at: String,
}

/// `meta.live.earlier_attempts`, present from the second attempt on.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AgentLiveEarlierAttempts {
    pub tokens_sent: Option<u64>,
    pub tokens_received: Option<u64>,
}

/// `meta.live` itself (`docs/plugins/agent.md`'s live-progress table)
/// -- a summary the agent tool writes through `Store.set` while its
/// session runs, parsed once here. A figure the CLI doesn't report
/// live stays `None`, never `0`: a front end reads that as a dash,
/// not a real zero.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentLive {
    pub engine: Option<String>,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub transcript: Option<String>,
    pub attempt: Option<u64>,
    pub phase: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub turns: Option<u64>,
    pub tool_calls: Option<u64>,
    pub tools: BTreeMap<String, AgentLiveToolUsage>,
    pub current_tool: Option<AgentLiveCurrentTool>,
    pub tokens_sent: Option<u64>,
    pub tokens_received: Option<u64>,
    pub cost_usd: Option<f64>,
    pub tokens_cached_in: Option<u64>,
    pub tokens_cache_write: Option<u64>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub turn_tokens: Vec<u64>,
    pub files_changed: Option<u64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    pub earlier_attempts: Option<AgentLiveEarlierAttempts>,
}

impl AgentLive {
    /// `None` for anything that isn't a `meta.live` object at all --
    /// every field inside it is read permissively (a wrong-typed or
    /// missing field just leaves that one field `None`/empty), since
    /// this is read every 100ms from a live, still-being-written
    /// process and must never turn a transient oddity into a panic or
    /// a stuck display.
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let str_field = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_string);
        let u64_field = |k: &str| obj.get(k).and_then(Value::as_u64);
        let f64_field = |k: &str| obj.get(k).and_then(Value::as_f64);
        let tools = obj
            .get("tools")
            .map(AgentLiveToolUsage::map_from_value)
            .unwrap_or_default();
        let current_tool = obj
            .get("current_tool")
            .filter(|v| !v.is_null())
            .and_then(|v| {
                let name = v.get("name").and_then(Value::as_str)?.to_string();
                let started_at = v.get("started_at").and_then(Value::as_str)?.to_string();
                Some(AgentLiveCurrentTool { name, started_at })
            });
        let turn_tokens = obj
            .get("turn_tokens")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let earlier_attempts = obj
            .get("earlier_attempts")
            .and_then(Value::as_object)
            .map(|e| AgentLiveEarlierAttempts {
                tokens_sent: e.get("tokens_sent").and_then(Value::as_u64),
                tokens_received: e.get("tokens_received").and_then(Value::as_u64),
            });
        Some(AgentLive {
            engine: str_field("engine"),
            model: str_field("model"),
            session_id: str_field("session_id"),
            transcript: str_field("transcript"),
            attempt: u64_field("attempt"),
            phase: str_field("phase"),
            started_at: str_field("started_at"),
            updated_at: str_field("updated_at"),
            turns: u64_field("turns"),
            tool_calls: u64_field("tool_calls"),
            tools,
            current_tool,
            tokens_sent: u64_field("tokens_sent"),
            tokens_received: u64_field("tokens_received"),
            cost_usd: f64_field("cost_usd"),
            tokens_cached_in: u64_field("tokens_cached_in"),
            tokens_cache_write: u64_field("tokens_cache_write"),
            context_tokens: u64_field("context_tokens"),
            context_window: u64_field("context_window"),
            turn_tokens,
            files_changed: u64_field("files_changed"),
            lines_added: u64_field("lines_added"),
            lines_removed: u64_field("lines_removed"),
            earlier_attempts,
        })
    }

    /// Issue #637's heartbeat rule: a session whose `updated_at` is
    /// within 5 seconds of `now` is running right now, even with no
    /// `--events` to confirm it. Once the process that would keep
    /// writing it is gone (an interrupted run's leftover `meta.live`),
    /// `updated_at` stops moving and this reads `false` within a few
    /// seconds -- the same leftover summary then reads as stopped,
    /// never as running, with no need to know anything about the
    /// run's own process state.
    pub fn is_recent(&self, now: &str) -> bool {
        let Some(updated_at) = self.updated_at.as_deref() else {
            return false;
        };
        crate::observe::duration_seconds(updated_at, now).is_some_and(|secs| secs.abs() <= 5.0)
    }
}

/// One effect node's `{value, meta}` shape, flattened to its dotted
/// state path (DESIGN.md §1.2-§1.3).
#[derive(Debug, Clone)]
pub struct NodeMeta {
    pub created_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    /// A named `if`'s `meta.branch` (`"then"`/`"else"`).
    pub branch: Option<String>,
    pub progress_done: Option<u64>,
    pub progress_total: Option<u64>,
    pub progress_eta_s: Option<f64>,
    /// `meta.live` (`docs/plugins/agent.md`'s "Live progress while the
    /// session runs"), parsed once here so every reader (the overview's
    /// now pane, Flow's selected pane, the Cost screen) shares one
    /// parse instead of re-reading the raw `Value` each time. `None`
    /// for every node that isn't a currently- or recently-live agent
    /// session, including one that finished normally (`meta.live` is
    /// removed once the attempt ends, success or failure, except when
    /// the run itself was interrupted).
    pub live: Option<AgentLive>,
    pub value: Value,
    pub meta: Value,
}

impl NodeMeta {
    fn from_object(value: &Value, meta: &Value) -> Self {
        NodeMeta {
            created_at: meta
                .get("created_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            completed_at: meta
                .get("completed_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            error: meta
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            branch: meta
                .get("branch")
                .and_then(Value::as_str)
                .map(str::to_string),
            progress_done: meta.pointer("/progress/done").and_then(Value::as_u64),
            progress_total: meta.pointer("/progress/total").and_then(Value::as_u64),
            progress_eta_s: meta.pointer("/progress/eta_s").and_then(Value::as_f64),
            live: meta.get("live").and_then(AgentLive::from_value),
            value: value.clone(),
            meta: meta.clone(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.completed_at.is_none()
    }

    pub fn is_ok(&self) -> bool {
        self.completed_at.is_some() && self.error.is_none()
    }
}

/// Walks a live-state (or final `--out`) snapshot's `prime` subtree,
/// returning every `{value, meta}` node keyed by its dotted state path
/// (DESIGN.md §1.3). `last` (a `$ref` alias to `iter_N`, §2.3) is
/// skipped — osi always resolves the real `iter_N` node instead.
pub fn flatten_state(state: &Value) -> BTreeMap<String, NodeMeta> {
    let mut out = BTreeMap::new();
    if let Some(prime) = state.get("prime") {
        walk(prime, "prime", &mut out);
    }
    out
}

fn walk(node: &Value, path: &str, out: &mut BTreeMap<String, NodeMeta>) {
    let Some(obj) = node.as_object() else {
        return;
    };
    if let (Some(value), Some(meta)) = (obj.get("value"), obj.get("meta")) {
        out.insert(path.to_string(), NodeMeta::from_object(value, meta));
    }
    for (key, child) in obj {
        if key == "value" || key == "meta" {
            continue;
        }
        // `last: {"$ref": "iter_N"}` is an alias (DESIGN.md §2.3):
        // skip it so osi always resolves the real `iter_N` node
        // instead — but only this exact shape. A real effect
        // genuinely named `last` (nothing stops an author writing
        // one) is an ordinary object with its own `value`/`meta`, not
        // `{"$ref": ...}`, and must still be walked (F13).
        if key == "last" && is_ref_alias(child) {
            continue;
        }
        let child_path = format!("{path}.{key}");
        walk(child, &child_path, out);
    }
}

fn is_ref_alias(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|obj| obj.len() == 1 && obj.contains_key("$ref"))
}

/// `runtime.last_run.completed_at` is set — the run has ended, whether
/// cleanly or not (DESIGN.md §2.1 rule 7).
pub fn run_ended(state: &Value) -> bool {
    state
        .pointer("/runtime/last_run/completed_at")
        .is_some_and(|v| !v.is_null())
}

/// `prime.meta.error == null` (DESIGN.md §2.1 rule 7).
///
/// Requires the key to be explicitly present and `null`, not merely
/// absent (K6): a run that failed *before* ever creating a `prime`
/// node at all (a document invalid enough that nothing ran) still
/// gets `runtime.last_run.completed_at` set in its final write, with
/// no `prime` key whatsoever — `/prime/meta/error` then fails to
/// resolve, and treating a missing pointer the same as an explicit
/// `null` printed `■ run ok` right before the engine's own nonzero
/// exit code. A nonzero exit is never "ok".
pub fn run_ok(state: &Value) -> bool {
    state
        .pointer("/prime/meta/error")
        .is_some_and(Value::is_null)
}

pub fn run_error(state: &Value) -> Option<String> {
    state
        .pointer("/prime/meta/error")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Rule 7's first bullet, shared by `run_status` (the TUI header) and
/// `Differ::finish` (the plain `--log` summary and `osi watch`'s own):
/// how an **ended** run's `prime.meta.error` reads, so the two
/// surfaces can never label the very same ended run differently.
/// `state` must already satisfy `run_ended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedOutcome {
    Ok,
    Cancelled,
    Failed,
}

pub fn ended_outcome(state: &Value) -> EndedOutcome {
    if run_ok(state) {
        return EndedOutcome::Ok;
    }
    match run_error(state) {
        Some(error) if error.starts_with("Interrupted") => EndedOutcome::Cancelled,
        _ => EndedOutcome::Failed,
    }
}

pub fn run_totals(state: &Value) -> Option<Value> {
    state.pointer("/runtime/last_run/totals").cloned()
}

/// `osi watch`'s own document name (issue #626): the watched run's
/// own `runtime.last_run.orchestration_path`, once a live-state
/// write has it -- `osi watch` otherwise has no document of its own
/// to name the header with at all, only the directory or file it was
/// pointed at.
pub fn orchestration_path(state: &Value) -> Option<&str> {
    state
        .pointer("/runtime/last_run/orchestration_path")
        .and_then(Value::as_str)
}

/// The token rule (DESIGN.md §6.3, issue #527): one node's own
/// `tokens_sent`/`tokens_received`, read from the `_total` field when
/// it is present and falling back to the plain one otherwise. Both
/// `cof` (`cli/runtime_shim.py`'s `_run_totals`/`_RunTotals.record`)
/// and electricity (`electricity/src/lib.rs`'s `RunTotals::record`)
/// build `runtime.last_run.totals` from exactly this pair, confirmed
/// by running each with the scripted adapter and a retried prompt
/// (electricity/docs/spec, `_total` present and equal to the plain
/// field there since a failed scripted attempt never carries tokens
/// of its own) — so every surface that reports tokens (the header,
/// the token bar, the state pane, the treemap) must read them the
/// same way, or a retried prompt would make them disagree. A
/// container's own `tokens_sent`/`tokens_received` are always `null`
/// (DESIGN.md §1.2), so this is zero for every non-leaf node without
/// any special-casing.
pub fn node_tokens(meta: &Value) -> (u64, u64) {
    let sent = meta
        .get("tokens_sent_total")
        .and_then(Value::as_u64)
        .or_else(|| meta.get("tokens_sent").and_then(Value::as_u64))
        .unwrap_or(0);
    let received = meta
        .get("tokens_received_total")
        .and_then(Value::as_u64)
        .or_else(|| meta.get("tokens_received").and_then(Value::as_u64))
        .unwrap_or(0);
    (sent, received)
}

/// Every entry at `path` itself or under it, `path.*`— the same set
/// a naive `flat.iter().filter(|(p, _)| p == path || p.starts_with(
/// &prefix))` scan would find, but in `O(log n + matches)` instead of
/// `O(n)`: this codebase's own path alphabet (effect names are ASCII
/// letters/digits/underscore, loop passes are `iter_N`) never puts a
/// character before `.` (0x2E) right after a complete segment, so in
/// a `BTreeMap`'s sorted order every match is contiguous, starting
/// exactly at `path` itself — nothing under a *different* child can
/// ever sort between two of `path`'s own descendants, and nothing
/// outside `path`'s subtree can sort between `path` and its first
/// child. Issue #527's own redraw-cost acceptance ("measure a
/// 2,000-node state") is what this exists for: called once per row,
/// the naive scan above is quadratic in the state's own size.
pub fn subtree_entries<'a, V>(
    path: &str,
    map: &'a BTreeMap<String, V>,
) -> impl Iterator<Item = (&'a String, &'a V)> {
    let prefix = format!("{path}.");
    map.range(path.to_string()..)
        .take_while(move |(p, _)| p.as_str() == path || p.starts_with(prefix.as_str()))
}

/// [`node_tokens`], summed over `path` and every one of its
/// descendants (DESIGN.md §6.3's state pane and treemap: "a container
/// ... shows the sum over its subtree"). For a leaf with no
/// descendants this is just its own `node_tokens`; a container's own
/// entry contributes nothing (its tokens are always `null`), so the
/// sum is exactly its children's.
pub fn subtree_tokens(path: &str, flat: &BTreeMap<String, NodeMeta>) -> (u64, u64) {
    subtree_entries(path, flat).fold((0u64, 0u64), |(sent, received), (_, node)| {
        let (s, r) = node_tokens(&node.meta);
        (sent + s, received + r)
    })
}

/// The token bar's "what's in flight" line (DESIGN.md §6.3, issue
/// #527): a prompt node counts as running only once its `prompt_sent`
/// has actually been written and it isn't still queued behind a
/// concurrency slot — a node that merely exists with `completed_at:
/// null` can be a tool, or a prompt that hasn't reached the adapter
/// call yet.
pub fn is_running_prompt(node: &NodeMeta) -> bool {
    node.created_at.is_some()
        && node.completed_at.is_none()
        && node
            .meta
            .get("prompt_sent")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && node.meta.get("waiting_for").is_none_or(|v| v.is_null())
}

/// The estimated input for one running prompt (DESIGN.md §6.3):
/// `ceil(chars(meta.prompt_sent) / 4)`.
pub fn estimated_prompt_tokens(prompt_sent: &str) -> u64 {
    let chars = prompt_sent.chars().count() as u64;
    chars.div_ceil(4)
}

/// Why a plan node was skipped (DESIGN.md §2.2). `Unknown` covers every
/// node osi infers as skipped without a more specific reason (e.g. one
/// matched by several candidate plan ops with conflicting reasons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    UntakenBranch,
    NotReached,
    CancelledSibling,
    Disabled,
    ZeroPasses,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Pending,
    Running,
    /// No node yet, but the enclosing chain-flow container is active
    /// and every earlier plan sibling is already complete (DESIGN.md
    /// §2.1 rule 1): the node may simply not have been written yet.
    LikelyRunning,
    /// A tree container's unfinished children: running or queued,
    /// indistinguishable from state alone (DESIGN.md §2.1 rule 4).
    RunningOrQueued,
    Done,
    Failed,
    FailedHandled,
    Skipped,
    Cancelled,
    Aborted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowStatus {
    pub kind: StatusKind,
    pub skip_reason: Option<SkipReason>,
    /// A tool node whose `created_at` moved forward while still running
    /// (DESIGN.md §2.1 rule 6). Prompts carry no such signal from state
    /// alone.
    pub retrying: bool,
}

impl RowStatus {
    fn new(kind: StatusKind) -> Self {
        RowStatus {
            kind,
            skip_reason: None,
            retrying: false,
        }
    }

    fn skipped(reason: SkipReason) -> Self {
        RowStatus {
            kind: StatusKind::Skipped,
            skip_reason: Some(reason),
            retrying: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Ok,
    Failed,
    Cancelled,
    Aborted,
}

/// Whether the engine process is still alive, and if not, how it ended
/// — inputs `RunModel` needs that don't come from a snapshot (DESIGN.md
/// §2.1 rules 2 and 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    /// The process exited.
    Exited {
        /// `prime.meta.error` starting with `"Interrupted"` — colours a
        /// still-open *row* cancelled vs aborted (rule 2). Not used for
        /// the run-wide status (`run_status`, rule 7): an ended run's
        /// own cancelled/failed split there comes from the state alone
        /// (`ended_outcome`), and an unended run has no single node to
        /// read this from at all.
        interrupted: bool,
        /// Rule 7's "no ended state" branch: a positive sign that this
        /// run specifically *failed*, not merely stopped — a real
        /// process exit code of exactly 1 for `osi run`/the TUI (what
        /// `cof` exits with for a pre-execution validation failure,
        /// DESIGN.md §4.1), or, for `osi watch` (which owns no process
        /// to read an exit code from), `--events`' own
        /// `run_end.ok == false`.
        likely_failed: bool,
        /// Whatever `failure_reason_fallback` already finds for this
        /// run: a `run_end` event's own error, or cof's pre-execution
        /// stdout JSON. Required alongside `likely_failed` — a stray
        /// leftover stderr line is not a failure reason on its own.
        has_failure_reason: bool,
        /// The time `osi run` itself saw the engine process exit --
        /// `None` for `osi watch`, which owns no process to observe,
        /// and for a caller with nothing useful to put here. The
        /// header's own elapsed time falls back to this, ahead of
        /// scanning state and events for a latest instant, once a
        /// run has ended with neither a `run_end` timestamp nor
        /// `prime.meta.completed_at` of its own.
        exited_at: Option<String>,
    },
}

impl ProcessState {
    /// `Exited` with no run-wide-failure information at all, for a
    /// caller that only needs the per-row `interrupted` colouring
    /// (rule 2), not `run_status` (rule 7).
    pub fn exited(interrupted: bool) -> Self {
        ProcessState::Exited {
            interrupted,
            likely_failed: false,
            has_failure_reason: false,
            exited_at: None,
        }
    }

    /// [`ProcessState::Exited`]'s own `exited_at`, when this is that
    /// variant and it has one.
    pub fn exited_at(&self) -> Option<&str> {
        match self {
            ProcessState::Exited { exited_at, .. } => exited_at.as_deref(),
            ProcessState::Running => None,
        }
    }
}

/// A tree container's own `dispatch` event (DESIGN.md §2.1 rule 4):
/// `branches` is the true total, `concurrency` the running ceiling when
/// the stream carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchInfo {
    pub branches: u64,
    pub concurrency: Option<u64>,
}

/// What `--events` says about one path, exact rather than inferred
/// (DESIGN.md §2.1's "With events (exact)" table) — `RunModel::observe`
/// overrides its own state-only estimate for a path wherever this
/// has an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventOutcome {
    /// A `start` is open, with no matching `end` yet and no `run_end`
    /// interruption to cancel it.
    Open,
    Done,
    Failed(Option<String>),
    /// A `start` was still open when an interrupted `run_end` arrived
    /// (DESIGN.md §2.1 rule "a start is still open when run_end
    /// arrives with an interruption").
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EventEnd {
    Ok,
    Err(Option<String>),
}

/// Tracks per-path status across a run's observations, from state alone
/// (DESIGN.md §2.1's "From state alone (best effort)" rules) plus
/// whatever `--events` has arrived (#419): a stream from a `cof`
/// without `--events` simply never calls `observe_event`, so every
/// status still comes from `observe`'s own state-only rules.
pub struct RunModel {
    last_created_at: BTreeMap<String, String>,
    /// A path's own `created_at` the last time it was seen not running
    /// with an error set -- a later running sighting with the *same*
    /// `created_at` is this pass retrying (#421: created_at no longer
    /// moves between a pass's own attempts, so `last_created_at`
    /// changing while running can no longer catch this).
    last_failed_created_at: BTreeMap<String, String>,
    dispatch: BTreeMap<String, DispatchInfo>,
    /// Open `start`s with an `id` (DESIGN.md §3: unique per effect
    /// *instance*), keyed by that id so an `end` with the same id pairs
    /// up even when several instances share one unnamed path.
    open_by_id: BTreeMap<i64, String>,
    /// Open `start`s with no `id` at all, counted per path rather than
    /// identified individually — still enough to know a path has *some*
    /// open instance.
    open_unid: BTreeMap<String, u32>,
    ended: BTreeMap<String, EventEnd>,
    /// A path's own latest `start` `ts`: a row events alone confirm
    /// `Running`, with no state node yet to read `created_at` from,
    /// still needs a start instant to measure its own elapsed time
    /// from.
    started_at: BTreeMap<String, String>,
    /// A path's own latest `end` `ms`: lets a row events alone
    /// confirm finished -- before state has caught up with a node
    /// for it at all -- carry a real duration, so it can fold into
    /// quick steps by the same under-a-second rule as any other row.
    ended_ms: BTreeMap<String, Option<u64>>,
    cancelled_by_run_end: BTreeSet<String>,
    run_start: Option<(String, Option<i32>)>,
    /// `run_start`'s own `ts` (DESIGN.md §3's format table) -- the
    /// run's own start, when `--events` carried one (issue #613's "one
    /// clock": header elapsed is measured from this, not from when
    /// osi itself started).
    run_start_ts: Option<String>,
    run_end: Option<(bool, Option<String>, Option<String>)>,
    /// `run_end`'s own `ts` -- the run's own end, when `--events`
    /// carried one.
    run_end_ts: Option<String>,
    /// The latest `ts` seen on any `start` or `end` event -- a run
    /// with no `run_end` and no `prime.meta.completed_at` of its own
    /// (an abort) still has this to end its elapsed time at, as long
    /// as something ever started or finished.
    latest_event_ts: Option<String>,
    /// The instant elapsed time froze at, the first time a run was
    /// seen as not running with no other known instant to end at
    /// all -- set once, by [`RunModel::freeze_end_ts`], and never
    /// moved again, so elapsed stops right there instead of ticking
    /// on to `now` on every later rebuild.
    frozen_end_ts: Option<String>,
    /// The latest report seen on any path at all (#610's header
    /// status line): once this is set, the header shows it in place
    /// of the run id, for the rest of the run.
    latest_report: Option<ReportSighting>,
    /// The latest report seen for each path (#610's details pane: "a
    /// named report's data comes from the latest report event for
    /// that path"). Keyed by the report's own path, which an unnamed
    /// report shares with its enclosing container -- the latest
    /// sighting at that path is still the right one to show if a
    /// caller ever looks it up by it.
    reports_by_path: BTreeMap<String, ReportSighting>,
    /// Every path a report event has ever named (#610's "no plan"
    /// fallback): `osi watch` and a `use: inline:`/`ref:` child have
    /// no compiled plan to resolve a report's own path from, so
    /// filtering a report's state node out of the tree falls back to
    /// this set instead of `PlanTree::is_report_path`.
    report_event_paths: BTreeSet<String>,
    /// `run_start`'s own `engine` field (issue #649's history record),
    /// when `--events` carried one -- both engines always write it,
    /// but a stream from an older build might not.
    run_start_engine: Option<String>,
    /// `Event::Start`'s own `ts`, by `id` -- `started_at` alone keeps
    /// only the latest start *per path*, which can't tell two
    /// concurrently open ids at the same (unnamed-loop) path apart.
    start_ts_by_id: BTreeMap<i64, String>,
    /// Every concrete path that has had at least one id-paired
    /// `start` observed via events -- `observe`'s own state-based
    /// interval capture (`push_state_interval`) skips any path in
    /// this set, since its events-sourced `end` already covers it
    /// exactly (or will once it arrives). Marked at `start`, not at
    /// `end`, so a redraw that observes the node already finished in
    /// state before the matching `end` line has been read still
    /// skips it, rather than logging the same pass from both
    /// sources.
    paths_with_id_events: BTreeSet<String>,
    /// One entry per effect instance that has run to completion,
    /// however it was observed (issue #649's history record: "a
    /// bounded log of intervals, keyed by event id"). Deduplicated so
    /// a redraw that re-observes the same finished state twice (the
    /// TUI calls `observe` on every tick) never pushes the same pass
    /// in again -- `recorded_state_intervals` for the state-sourced
    /// half, `id` uniqueness for the events-sourced half.
    step_intervals: Vec<StepInterval>,
    /// `(path, created_at)` pairs already turned into a
    /// `step_intervals` entry by `observe`'s own state-based capture
    /// -- the dedup key for that half of the log.
    recorded_state_intervals: BTreeSet<(String, String)>,
    /// Once [`MAX_STEP_INTERVALS`] entries have been logged, a
    /// dropped pass's own path is no longer kept (issue #649: "then
    /// keeps only counts per minute") -- just how many of its own
    /// start/end events fell in each clock minute, keyed by the
    /// first 16 characters of the timestamp (`"2026-10-08T11:52"`,
    /// true of both an RFC-3339 event timestamp and state's own
    /// microsecond-plus-offset one), for the Time screen's events/
    /// min strip to keep covering a run this large.
    overflowed_minute_counts: BTreeMap<String, u32>,
    /// How many passes [`RunModel::step_intervals`] has dropped in
    /// total past the cap -- since their own paths are no longer
    /// kept, a caller building a history record folds this whole
    /// count into `(other)` rather than attributing it to any one
    /// step.
    overflowed_total_passes: u64,
}

/// One effect instance's own start/end, as observed either from
/// events (exact) or from state's own `created_at`/`completed_at`
/// (best effort) -- [`RunModel::step_intervals`]'s own entry shape,
/// and issue #649 history record's per-step input.
#[derive(Debug, Clone, PartialEq)]
pub struct StepInterval {
    pub path: String,
    pub start_ts: String,
    pub end_ts: String,
    pub ok: bool,
    /// `None` for an events-sourced interval (DESIGN.md: events carry
    /// no token/cost figures at all) -- state-sourced intervals
    /// always have a number here, `0` when the node reported none.
    pub tokens_sent: Option<u64>,
    pub tokens_received: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// [`RunModel::step_intervals`]'s own bound (issue #649).
pub(crate) const MAX_STEP_INTERVALS: usize = 50_000;

/// One `report` event, as last seen (#610). `ts`/`level`/`message`
/// are the header status line and the details pane's own fields;
/// `data`/`data_truncated` are the details pane's only (a header
/// status line never shows a report's data).
#[derive(Debug, Clone, PartialEq)]
pub struct ReportSighting {
    pub ts: String,
    pub level: crate::observe::ReportLevel,
    pub message: String,
    pub data: Option<Value>,
    pub data_truncated: bool,
}

impl RunModel {
    pub fn new() -> Self {
        RunModel {
            last_created_at: BTreeMap::new(),
            last_failed_created_at: BTreeMap::new(),
            dispatch: BTreeMap::new(),
            open_by_id: BTreeMap::new(),
            open_unid: BTreeMap::new(),
            ended: BTreeMap::new(),
            started_at: BTreeMap::new(),
            ended_ms: BTreeMap::new(),
            cancelled_by_run_end: BTreeSet::new(),
            run_start: None,
            run_start_ts: None,
            run_end: None,
            run_end_ts: None,
            latest_event_ts: None,
            frozen_end_ts: None,
            latest_report: None,
            reports_by_path: BTreeMap::new(),
            report_event_paths: BTreeSet::new(),
            run_start_engine: None,
            start_ts_by_id: BTreeMap::new(),
            paths_with_id_events: BTreeSet::new(),
            step_intervals: Vec::new(),
            recorded_state_intervals: BTreeSet::new(),
            overflowed_minute_counts: BTreeMap::new(),
            overflowed_total_passes: 0,
        }
    }

    /// Feeds one parsed `--events` line in (DESIGN.md §2.1's "With
    /// events (exact)" table, §3's ordering/abort rules).
    pub fn observe_event(&mut self, event: &Event) {
        match event {
            Event::Dispatch {
                path,
                branches,
                concurrency,
                ..
            } => {
                self.dispatch.insert(
                    path.clone(),
                    DispatchInfo {
                        branches: *branches,
                        concurrency: *concurrency,
                    },
                );
            }
            Event::RunStart {
                ts,
                run_id,
                pid,
                engine,
            } => {
                self.run_start = Some((run_id.clone(), *pid));
                self.run_start_ts = Some(ts.clone());
                self.run_start_engine = engine.clone();
            }
            Event::Start { ts, id, path } => {
                self.started_at.insert(path.clone(), ts.clone());
                self.note_event_ts(ts);
                self.ended_ms.remove(path);
                // A fresh `start` at a path this instance already saw
                // `end` for is a retry (or the next tree-flow instance
                // reusing an unnamed path, DESIGN.md §2.3): either way
                // it is open again now, so a stale `ended` entry must
                // not keep answering for it.
                self.ended.remove(path);
                match id {
                    Some(id) => {
                        self.open_by_id.insert(*id, path.clone());
                        self.start_ts_by_id.insert(*id, ts.clone());
                        self.paths_with_id_events.insert(path.clone());
                    }
                    None => {
                        *self.open_unid.entry(path.clone()).or_insert(0) += 1;
                    }
                }
            }
            Event::End {
                ts,
                id,
                path,
                ok,
                ms,
                error,
            } => {
                self.ended_ms.insert(path.clone(), *ms);
                self.note_event_ts(ts);
                match id {
                    Some(id) => {
                        self.open_by_id.remove(id);
                        if let Some(start_ts) = self.start_ts_by_id.remove(id) {
                            self.push_step_interval(StepInterval {
                                path: path.clone(),
                                start_ts,
                                end_ts: ts.clone(),
                                ok: *ok,
                                tokens_sent: None,
                                tokens_received: None,
                                cost_usd: None,
                            });
                        }
                    }
                    None => {
                        // An `end` with `id: null` can still name a
                        // path whose own `start` carried an id (issue
                        // #433): close the oldest open id'd start at
                        // that path, since `open_by_id` is a
                        // `BTreeMap` and ids are strictly increasing
                        // in file order, so the first match found by
                        // iterating it is the oldest. Otherwise it
                        // would stay open forever, and the row it
                        // belongs to would end the run looking
                        // cancelled/aborted instead of however it
                        // really ended.
                        let oldest = self
                            .open_by_id
                            .iter()
                            .find(|(_, p)| p.as_str() == path)
                            .map(|(id, _)| *id);
                        if let Some(oldest_id) = oldest {
                            self.open_by_id.remove(&oldest_id);
                        } else if let Some(count) = self.open_unid.get_mut(path) {
                            *count = count.saturating_sub(1);
                        }
                    }
                }
                self.ended.insert(
                    path.clone(),
                    if *ok {
                        EventEnd::Ok
                    } else {
                        EventEnd::Err(error.clone())
                    },
                );
            }
            Event::RunEnd {
                ts,
                ok,
                error,
                signal,
            } => {
                self.run_end = Some((*ok, error.clone(), signal.clone()));
                self.run_end_ts = Some(ts.clone());
                let interrupted = error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("Interrupted"));
                if interrupted {
                    for path in self.open_by_id.values() {
                        self.cancelled_by_run_end.insert(path.clone());
                    }
                    for (path, count) in &self.open_unid {
                        if *count > 0 {
                            self.cancelled_by_run_end.insert(path.clone());
                        }
                    }
                }
                self.open_by_id.clear();
                self.open_unid.clear();
            }
            Event::Report {
                ts,
                path,
                level,
                message,
                data,
                data_truncated,
            } => {
                let sighting = ReportSighting {
                    ts: ts.clone(),
                    level: *level,
                    message: message.clone(),
                    data: data.clone(),
                    data_truncated: *data_truncated,
                };
                self.latest_report = Some(sighting.clone());
                self.reports_by_path.insert(path.clone(), sighting);
                self.report_event_paths.insert(path.clone());
            }
        }
    }

    /// The latest report seen on any path (#610's header status
    /// line) -- `None` until the first one arrives, and never cleared
    /// again afterwards.
    pub fn latest_report(&self) -> Option<&ReportSighting> {
        self.latest_report.as_ref()
    }

    /// The latest report seen for `path` specifically (#610's details
    /// pane), when `--events` has carried one.
    pub fn report_at(&self, path: &str) -> Option<&ReportSighting> {
        self.reports_by_path.get(path)
    }

    /// Whether `path` is report-only by the "no plan" fallback:
    /// `osi watch` and a `use: inline:`/`ref:` child have no compiled
    /// plan to resolve a report's own path from, so the paths seen on
    /// `--events` stand in for `PlanTree::is_report_path` there. Only
    /// when the plan genuinely has nothing to say about `path` at
    /// all (`plan.match_path` finds no entries) and the node's own
    /// value has the report shape -- a path with a plan entry (a
    /// named `if`, `dynamic` or `use` child whose first thing is an
    /// unnamed report) is a real row this must never hide, even
    /// though the unnamed report's own event also named that path.
    pub fn is_report_event_path(&self, path: &str, plan: &PlanTree, value: &Value) -> bool {
        self.report_event_paths.contains(path)
            && plan.match_path(path).is_none_or(|m| m.entries.is_empty())
            && crate::render::is_report_value(value)
    }

    /// Whether a `run_end` line has arrived yet (DESIGN.md §3: when it
    /// has, the final live-state write is already on disk) — `osi
    /// watch`'s own stop condition alongside a completed state (F4).
    pub fn run_ended_by_events(&self) -> bool {
        self.run_end.is_some()
    }

    /// `run_end`'s own `ok`, when one has arrived — lets a caller settle
    /// a completed run's exit status from the event itself rather than
    /// re-deriving it from state (F4).
    pub fn run_end_ok(&self) -> Option<bool> {
        self.run_end.as_ref().map(|(ok, _, _)| *ok)
    }

    /// `run_end`'s own `signal` field (DESIGN.md §3's format table),
    /// when one has arrived and the run ended by a forwarded signal
    /// rather than cleanly — lets `osi watch` report the same exit
    /// code `cof` itself used (130/143/129), not just ok-vs-failed
    /// (P2-7).
    pub fn run_end_signal(&self) -> Option<&str> {
        self.run_end
            .as_ref()
            .and_then(|(_, _, signal)| signal.as_deref())
    }

    /// `run_end`'s own `error` (M2): the engine's own reason for a run
    /// that failed before it ever wrote a usable `prime.meta.error` of
    /// its own -- the one source DESIGN.md §3's format table always
    /// gives a failing run_end a value for, ahead of reparsing cof's
    /// own pre-execution stdout JSON.
    pub fn run_end_error(&self) -> Option<&str> {
        self.run_end
            .as_ref()
            .and_then(|(_, error, _)| error.as_deref())
    }

    /// The engine's own pid, from `run_start` (DESIGN.md §3's format
    /// table), when an events stream carried one — lets `osi watch`
    /// tell a genuine abort (no `run_end`, dead process) from a run
    /// simply still going, with no child handle of its own (F4).
    pub fn run_start_pid(&self) -> Option<i32> {
        self.run_start.as_ref().and_then(|(_, pid)| *pid)
    }

    /// The run id a `run_start` event announced, when one arrived.
    pub fn run_start_id(&self) -> Option<&str> {
        self.run_start.as_ref().map(|(run_id, _)| run_id.as_str())
    }

    /// The run's own start timestamp, from `run_start`'s own `ts`
    /// (issue #613's "one clock"), when `--events` carried one.
    pub fn run_start_ts(&self) -> Option<&str> {
        self.run_start_ts.as_deref()
    }

    /// The run's own end timestamp, from `run_end`'s own `ts`, once
    /// one has arrived.
    pub fn run_end_ts(&self) -> Option<&str> {
        self.run_end_ts.as_deref()
    }

    /// The latest `ts` of any `start` or `end` event seen so far --
    /// compared as real instants, never as raw strings, since an
    /// event's own millisecond-plus-`Z` timestamp isn't
    /// lexicographically comparable against another one truncated at
    /// a different precision.
    pub fn latest_event_ts(&self) -> Option<&str> {
        self.latest_event_ts.as_deref()
    }

    /// Freezes the header's own elapsed time at `now`, the first
    /// time a run is seen as not running with no run-end, completion,
    /// process-exit or event instant to use instead -- every later
    /// call, with any `now`, returns that same frozen instant.
    pub fn freeze_end_ts(&mut self, now: &str) -> &str {
        self.frozen_end_ts.get_or_insert_with(|| now.to_string())
    }

    fn note_event_ts(&mut self, ts: &str) {
        let newer = match (&self.latest_event_ts, crate::diff::parse_instant(ts)) {
            (Some(prev), Some(parsed)) => {
                crate::diff::parse_instant(prev).is_none_or(|p| parsed > p)
            }
            (None, _) => true,
            _ => false,
        };
        if newer {
            self.latest_event_ts = Some(ts.to_string());
        }
    }

    /// Every path `--events` has said anything about at all — the set
    /// `observe` overlays its exact statuses onto, including a tree
    /// branch or `use` child never visible in state at all while it
    /// runs (DESIGN.md §1.4).
    fn event_paths(&self) -> BTreeSet<String> {
        self.open_by_id
            .values()
            .cloned()
            .chain(self.open_unid.keys().cloned())
            .chain(self.ended.keys().cloned())
            .chain(self.cancelled_by_run_end.iter().cloned())
            .collect()
    }

    /// `path`'s exact status from events alone (DESIGN.md §2.1's "With
    /// events" table), or `None` when `--events` has never mentioned
    /// it — `observe`'s state-only estimate stands uncontested then.
    fn event_status(&self, path: &str) -> Option<EventOutcome> {
        if self.cancelled_by_run_end.contains(path) {
            return Some(EventOutcome::Cancelled);
        }
        let open = self.open_by_id.values().any(|p| p == path)
            || self.open_unid.get(path).is_some_and(|c| *c > 0);
        if open {
            return Some(EventOutcome::Open);
        }
        match self.ended.get(path) {
            Some(EventEnd::Ok) => Some(EventOutcome::Done),
            Some(EventEnd::Err(error)) => Some(EventOutcome::Failed(error.clone())),
            None => None,
        }
    }

    /// The most recent `dispatch` event recorded for `path`, if any.
    pub fn dispatch_info(&self, path: &str) -> Option<DispatchInfo> {
        self.dispatch.get(path).copied()
    }

    /// The exact running-or-queued bound for a tree container
    /// (DESIGN.md §2.1 rule 4): its `dispatch` event's `concurrency`
    /// when the stream has it, else its `branches` (every unfinished
    /// child could in principle be running at once). `None` without a
    /// `dispatch` event at all — the state-only estimate (`max_concurrency`
    /// from the plan, or "≤ cpu-dependent" with none) is a renderer's
    /// own fallback, not this method's job.
    pub fn running_or_queued_bound(&self, path: &str) -> Option<u64> {
        let info = self.dispatch_info(path)?;
        Some(info.concurrency.unwrap_or(info.branches))
    }

    /// `path`'s own latest `start` event `ts` -- a renderer's own
    /// fallback for a row events alone confirm running, before any
    /// state node exists for it to read `created_at` from at all.
    pub fn event_started_at(&self, path: &str) -> Option<&str> {
        self.started_at.get(path).map(String::as_str)
    }

    /// `path`'s own latest `end` event duration, in seconds: `None`
    /// either with no matching `end` event yet, or with one that
    /// carried no `ms` of its own. A renderer's own fallback for a
    /// row events alone confirm finished, before state has caught up
    /// with a node for it at all.
    pub fn event_duration_s(&self, path: &str) -> Option<f64> {
        self.ended_ms
            .get(path)
            .copied()
            .flatten()
            .map(|ms| ms as f64 / 1000.0)
    }

    /// Pushes an events-sourced interval (an id-paired `start`/`end`).
    fn push_step_interval(&mut self, interval: StepInterval) {
        if self.step_intervals.len() >= MAX_STEP_INTERVALS {
            self.overflowed_total_passes += 1;
            self.bump_overflowed_minute(&interval.start_ts);
            self.bump_overflowed_minute(&interval.end_ts);
            return;
        }
        self.step_intervals.push(interval);
    }

    /// Counts one dropped interval's own start or end event into its
    /// clock minute (`overflowed_minute_counts`'s own key, the first
    /// 16 characters of `ts`).
    fn bump_overflowed_minute(&mut self, ts: &str) {
        let minute = ts.get(..16).unwrap_or(ts).to_string();
        *self.overflowed_minute_counts.entry(minute).or_insert(0) += 1;
    }

    /// Pushes a state-sourced interval (`observe`'s own "just finished"
    /// hook) -- a no-op when `path` already has id-paired event
    /// coverage, or this exact `(path, created_at)` pass was already
    /// recorded on an earlier redraw.
    fn push_state_interval(
        &mut self,
        path: &str,
        created_at: &str,
        completed_at: &str,
        node: &NodeMeta,
    ) {
        if self.paths_with_id_events.contains(path) {
            return;
        }
        let key = (path.to_string(), created_at.to_string());
        if self.recorded_state_intervals.contains(&key) {
            return;
        }
        self.recorded_state_intervals.insert(key);
        let (tokens_sent, tokens_received) = node_tokens(&node.meta);
        self.push_step_interval(StepInterval {
            path: path.to_string(),
            start_ts: created_at.to_string(),
            end_ts: completed_at.to_string(),
            ok: node.is_ok(),
            tokens_sent: Some(tokens_sent),
            tokens_received: Some(tokens_received),
            cost_usd: node.meta.get("cost_usd").and_then(Value::as_f64),
        });
    }

    /// Every interval logged so far (issue #649's history record) --
    /// at most [`MAX_STEP_INTERVALS`]; [`RunModel::overflowed_minute_counts`]
    /// and [`RunModel::overflowed_total_passes`] cover the rest, by
    /// clock minute and in total, with no per-path timing.
    pub fn step_intervals(&self) -> &[StepInterval] {
        &self.step_intervals
    }

    /// How many passes [`RunModel::step_intervals`] has dropped past
    /// its own cap, in total -- a caller building a history record
    /// folds this whole count into `(other)`, since no per-path
    /// breakdown survives the cap any more.
    pub fn overflowed_total_passes(&self) -> u64 {
        self.overflowed_total_passes
    }

    /// Counts of dropped start/end events per clock minute, past
    /// [`RunModel::step_intervals`]'s own cap -- the Time screen's
    /// events/min strip's own tail for a run large enough to reach
    /// it. Keyed by the first 16 characters of the timestamp
    /// (`"2026-10-08T11:52"`).
    pub fn overflowed_minute_counts(&self) -> &BTreeMap<String, u32> {
        &self.overflowed_minute_counts
    }

    /// `run_start`'s own `engine` field (issue #649), when `--events`
    /// carried one.
    pub fn run_start_engine(&self) -> Option<&str> {
        self.run_start_engine.as_deref()
    }

    /// Whether `--events` has actually been flowing at all (any
    /// `start`/`end` line seen) -- the Time screen's events/min
    /// strip reads this to decide between counting logged intervals
    /// and falling back to state's own `created_at`/`completed_at`
    /// (DESIGN.md: "without `--events`, osi counts state's ...
    /// instead and labels the strip `(state)`").
    pub fn has_events(&self) -> bool {
        self.latest_event_ts.is_some()
    }

    /// Recomputes every row's status from one snapshot, the plan, and
    /// the process's current state. Rows for plan paths with no node in
    /// `state` are still produced; `use: inline`/reflector-generated
    /// paths that have no plan entry are added as rows too, since they
    /// come from `state` alone.
    pub fn observe(
        &mut self,
        state: &Value,
        plan: &PlanTree,
        process: ProcessState,
    ) -> BTreeMap<String, RowStatus> {
        let flat = flatten_state(state);
        let mut rows = BTreeMap::new();

        for (path, node) in &flat {
            // #610: a named report writes a state node (#605), but a
            // report never fires start/end and so never becomes a
            // plan row or counts toward effects_planned/effects_done
            // (`plan::record_op_entry`) -- its state node must not
            // become a tree row either, or it reads like a step and
            // changes folding. With a plan, `PlanTree::is_report_path`
            // resolves this exactly, `iter_*`/grafted `use` children
            // included; with none (`osi watch`, a `use: inline:`/
            // `ref:` child), `--events`' own report paths are the
            // only source of truth there is -- `is_report_event_path`
            // only trusts them where the plan has no real entry at
            // that path and the node's own value has the report
            // shape, so a named `if`/`dynamic`/`use` whose first
            // thing is an unnamed report keeps its own row.
            // `prime` itself is excluded here so an unnamed top-level
            // report never hides the root's own real row; the same
            // ambiguity for an unnamed report directly inside some
            // other *named* container with no plan at all is a
            // narrower, accepted gap (#610).
            if plan.is_report_path(path)
                || (path != "prime" && self.is_report_event_path(path, plan, &node.value))
            {
                continue;
            }
            let retrying = if node.is_running() {
                let moved = match (self.last_created_at.get(path), &node.created_at) {
                    (Some(prev), Some(now)) => prev != now,
                    _ => false,
                };
                let same_pass_retry =
                    match (self.last_failed_created_at.get(path), &node.created_at) {
                        (Some(failed_ca), Some(now)) => failed_ca == now,
                        _ => false,
                    };
                moved || same_pass_retry
            } else {
                false
            };
            if let Some(created_at) = &node.created_at {
                self.last_created_at
                    .insert(path.clone(), created_at.clone());
            }
            if !node.is_running() {
                if node.error.is_some() {
                    if let Some(created_at) = &node.created_at {
                        self.last_failed_created_at
                            .insert(path.clone(), created_at.clone());
                    }
                } else {
                    self.last_failed_created_at.remove(path);
                }
                // Issue #649's history record: a path whose own pass
                // has no id-paired event (no `--events` at all, or an
                // unnamed `start`/`end`) gets its interval from state
                // alone, captured right here, exactly once per
                // `created_at` -- the only moment a reused (unnamed-
                // loop) path's finished pass is still visible before
                // the next one overwrites it.
                if let (Some(created_at), Some(completed_at)) =
                    (&node.created_at, &node.completed_at)
                {
                    self.push_state_interval(path, created_at, completed_at, node);
                }
            }

            let kind = if node.is_running() {
                match &process {
                    ProcessState::Running => StatusKind::Running,
                    ProcessState::Exited {
                        interrupted: true, ..
                    } => StatusKind::Cancelled,
                    ProcessState::Exited {
                        interrupted: false, ..
                    } => StatusKind::Aborted,
                }
            } else if node.is_ok() {
                StatusKind::Done
            } else {
                classify_failure(path, plan)
            };

            rows.insert(
                path.clone(),
                RowStatus {
                    kind,
                    skip_reason: None,
                    retrying,
                },
            );
        }

        if plan.has_plan() {
            for path in plan.all_paths() {
                if rows.contains_key(path.as_str()) {
                    continue;
                }
                // A named loop's body is listed once per compiled
                // pass *template* (`display_path` renders every `Pass`
                // segment as the literal string `iter_*`, DESIGN.md
                // §5's path-display ask) — `match_path` can never match
                // that literal text back against a real `iter_N`
                // segment, so seeding a row for it here only ever left
                // a stale `Pending` row beside the real per-pass rows
                // observation produces (F8). Skip it: a real pass's own
                // row already covers it, and an unreached/skipped loop
                // shows via the loop container's own row, not a body
                // template's.
                if path.split('.').any(|seg| seg == "iter_*") {
                    continue;
                }
                rows.insert(path.clone(), self.infer_absent(path, &flat, plan));
            }
        }

        // DESIGN.md §2.1's "With events (exact)" table overrides the
        // state-only rules above wherever `--events` has an answer for
        // a path — including a tree branch or `use` child never visible
        // in state at all while it runs (§1.4), which the loops above
        // never produce a row for in the first place.
        for path in self.event_paths() {
            let kind = match self.event_status(&path) {
                Some(EventOutcome::Open) => match &process {
                    ProcessState::Running => StatusKind::Running,
                    // No `run_end` ever cancelled this open start
                    // (`event_status` would have said `Cancelled`
                    // instead), yet the process has already exited:
                    // DESIGN.md §2.1's "no run_end and the process
                    // exited" rule.
                    ProcessState::Exited { .. } => StatusKind::Aborted,
                },
                Some(EventOutcome::Cancelled) => StatusKind::Cancelled,
                Some(EventOutcome::Done) => StatusKind::Done,
                Some(EventOutcome::Failed(_)) => classify_failure(&path, plan),
                None => continue,
            };
            rows.insert(
                path,
                RowStatus {
                    kind,
                    skip_reason: None,
                    retrying: false,
                },
            );
        }

        rows
    }

    /// DESIGN.md §2.1 rule 1 / §2.2: a plan path with no node at all.
    fn infer_absent(
        &self,
        path: &str,
        flat: &BTreeMap<String, NodeMeta>,
        plan: &PlanTree,
    ) -> RowStatus {
        let Some(m) = plan.match_path(path) else {
            return RowStatus::new(StatusKind::Pending);
        };
        let Some(entry) = m.entries.first() else {
            return RowStatus::new(StatusKind::Pending);
        };

        if !entry.enabled {
            return RowStatus::skipped(SkipReason::Disabled);
        }

        // Finding 9: an unnamed `if`'s own branches have no node to
        // report `meta.branch` from the way a *named* if's own does —
        // but once any path on the *other* side has a real node, this
        // one, still absent, is definitely the branch that didn't run,
        // not merely one the chain-flow heuristic below hasn't caught
        // up to yet.
        if plan
            .branch_alternates(path)
            .iter()
            .any(|alt| flat.contains_key(alt))
        {
            return RowStatus::skipped(SkipReason::UntakenBranch);
        }

        // An ancestor is complete and this path never appeared: skipped,
        // with a reason taken from the nearest complete ancestor
        // (DESIGN.md §2.2). A parent with no node yet either defers to
        // its own inferred status, recursively.
        if let Some(parent) = parent_path(path) {
            if let Some(parent_node) = flat.get(&parent) {
                if !parent_node.is_running() {
                    return self.skip_reason_for(parent_node);
                }
            } else if plan.match_path(&parent).is_some() {
                return self.infer_absent(&parent, flat, plan);
            }
        }

        // Chain-flow heuristic (DESIGN.md §2.1 rule 1): the enclosing
        // container is still running (or is the root itself), *and*
        // every earlier sibling in that same chain is already done one
        // way or another — otherwise this node hasn't been reached yet
        // regardless of what the container is doing, and is simply
        // Pending (F8: the bound below used to skip the sibling check
        // entirely and call every such node LikelyRunning).
        if entry.parent_flow == Some(Flow::Chain)
            && plan
                .earlier_siblings(path)
                .iter()
                .all(|sibling| self.sibling_is_complete(sibling, flat, plan))
        {
            if let Some(parent) = parent_path(path) {
                if flat.get(&parent).is_none_or(|n| n.is_running()) {
                    return RowStatus::new(StatusKind::LikelyRunning);
                }
            } else {
                return RowStatus::new(StatusKind::LikelyRunning);
            }
        }

        // Tree-flow rule (DESIGN.md §2.1 rule 4): an unfinished child of
        // a *running* tree container is running or queued, and the two
        // can't be told apart from state alone. Unlike the chain-flow
        // heuristic above, a container that hasn't even started yet
        // (absent from state) leaves its children merely pending — tree
        // dynamics/`each` loops do write their own node promptly (§1.3),
        // so its total absence is a real signal, not a write-lag gap.
        if entry.parent_flow == Some(Flow::Tree) {
            if let Some(parent) = parent_path(path) {
                if flat.get(&parent).is_some_and(NodeMeta::is_running) {
                    return RowStatus::new(StatusKind::RunningOrQueued);
                }
            }
        }

        RowStatus::new(StatusKind::Pending)
    }

    /// Whether `sibling` has finished one way or another — done,
    /// failed, or skipped — so a later chain sibling could plausibly
    /// have started (DESIGN.md §2.1 rule 1). Recurses through
    /// `infer_absent` for a sibling that hasn't appeared in state at
    /// all yet, rather than treating "absent" as automatically
    /// incomplete: a *disabled* or branch-skipped sibling never gets a
    /// node either, and still lets the chain move on.
    fn sibling_is_complete(
        &self,
        sibling: &str,
        flat: &BTreeMap<String, NodeMeta>,
        plan: &PlanTree,
    ) -> bool {
        if let Some(node) = flat.get(sibling) {
            return !node.is_running();
        }
        matches!(
            self.infer_absent(sibling, flat, plan).kind,
            StatusKind::Skipped | StatusKind::Done | StatusKind::Failed | StatusKind::FailedHandled
        )
    }

    /// DESIGN.md §2.2, from a complete ancestor's own node: an untaken
    /// `if` branch (`meta.branch` names the other side), a chain
    /// sibling's failure (`meta.error` set), or no more specific reason
    /// available from state alone.
    fn skip_reason_for(&self, parent_node: &NodeMeta) -> RowStatus {
        if parent_node.branch.is_some() {
            return RowStatus::skipped(SkipReason::UntakenBranch);
        }
        if parent_node.error.is_some() {
            return RowStatus::skipped(SkipReason::NotReached);
        }
        RowStatus::skipped(SkipReason::Unknown)
    }
}

impl Default for RunModel {
    fn default() -> Self {
        Self::new()
    }
}

fn parent_path(path: &str) -> Option<String> {
    path.rsplit_once('.').map(|(parent, _)| parent.to_string())
}

/// `failed` or `failed (handled)`, from the plan's own `on_error`
/// (DESIGN.md §2.1 rule 3 and the events table's matching row) —
/// shared between the state-only path and the events overlay so the
/// two rules can't drift apart.
fn classify_failure(path: &str, plan: &PlanTree) -> StatusKind {
    match plan.match_path(path).and_then(|m| m.entries.first()) {
        Some(entry)
            if matches!(
                entry.on_error,
                electricity_bytecode::OnError::Skip | electricity_bytecode::OnError::Continue
            ) =>
        {
            StatusKind::FailedHandled
        }
        _ => StatusKind::Failed,
    }
}

/// Status of the run as a whole (DESIGN.md §2.1 rule 7), shared by
/// the TUI header, the plain `--log` summary and `osi watch`: all
/// three derive the same answer from the same inputs, rather than
/// each guessing from `prime.meta.error` on its own.
///
/// - Running while the process is alive.
/// - An **ended** run (`runtime.last_run.completed_at` set): `ok` if
///   `prime.meta.error` is `null`, **cancelled** if it starts with
///   `"Interrupted"`, otherwise **failed** (`ended_outcome`).
/// - A process exited with no ended state: **failed** only when the
///   caller's `likely_failed` and `has_failure_reason` both hold
///   (a real pre-execution validation failure); otherwise **aborted**
///   (a second signal, SIGKILL, a crash: no final write).
pub fn run_status(state: Option<&Value>, process: ProcessState) -> RunStatus {
    match process {
        ProcessState::Running => RunStatus::Running,
        ProcessState::Exited {
            likely_failed,
            has_failure_reason,
            ..
        } => {
            if let Some(state) = state {
                if run_ended(state) {
                    return match ended_outcome(state) {
                        EndedOutcome::Ok => RunStatus::Ok,
                        EndedOutcome::Cancelled => RunStatus::Cancelled,
                        EndedOutcome::Failed => RunStatus::Failed,
                    };
                }
            }
            if likely_failed && has_failure_reason {
                RunStatus::Failed
            } else {
                RunStatus::Aborted
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::Event;
    use serde_json::json;

    #[test]
    fn agent_live_parses_every_documented_field() {
        let live = AgentLive::from_value(&json!({
            "v": 1,
            "engine": "pi",
            "model": "sonnet",
            "session_id": "s1",
            "transcript": "/tmp/t.jsonl",
            "attempt": 1,
            "phase": "turn",
            "started_at": "2026-10-08T11:46:43Z",
            "updated_at": "2026-10-08T11:52:55Z",
            "turns": 14,
            "tool_calls": 31,
            "tools": {"bash": {"calls": 3, "ms": 1200}},
            "current_tool": {"name": "bash", "started_at": "2026-10-08T11:52:30Z"},
            "tokens_sent": 2100000,
            "tokens_received": 18000,
            "cost_usd": 1.10,
            "tokens_cached_in": 2000000,
            "tokens_cache_write": 50000,
            "context_tokens": 142000,
            "context_window": 200000,
            "turn_tokens": [100, 200, 300],
            "files_changed": 3,
            "lines_added": 12,
            "lines_removed": 3
        }))
        .unwrap();
        assert_eq!(live.engine, Some("pi".to_string()));
        assert_eq!(live.model, Some("sonnet".to_string()));
        assert_eq!(live.turns, Some(14));
        assert_eq!(live.tool_calls, Some(31));
        assert_eq!(
            live.tools.get("bash"),
            Some(&AgentLiveToolUsage { calls: 3, ms: 1200 })
        );
        assert_eq!(
            live.current_tool,
            Some(AgentLiveCurrentTool {
                name: "bash".to_string(),
                started_at: "2026-10-08T11:52:30Z".to_string()
            })
        );
        assert_eq!(live.tokens_sent, Some(2_100_000));
        assert_eq!(live.tokens_received, Some(18_000));
        assert_eq!(live.cost_usd, Some(1.10));
        assert_eq!(live.tokens_cached_in, Some(2_000_000));
        assert_eq!(live.context_tokens, Some(142_000));
        assert_eq!(live.context_window, Some(200_000));
        assert_eq!(live.turn_tokens, vec![100, 200, 300]);
        assert_eq!(live.files_changed, Some(3));
        assert_eq!(live.lines_added, Some(12));
        assert_eq!(live.lines_removed, Some(3));
        assert_eq!(live.earlier_attempts, None);
    }

    /// `docs/plugins/agent.md`: a figure the CLI doesn't report live
    /// (Claude Code's `tokens_received`/`cost_usd`) is absent from
    /// `meta.live` entirely, not written as `0` -- parsing must leave
    /// it `None`, the one shape a front end reads as a dash rather
    /// than a real zero.
    #[test]
    fn a_figure_not_reported_live_parses_as_none_not_zero() {
        let live = AgentLive::from_value(&json!({
            "engine": "claude_code",
            "tokens_sent": 1600,
            "tokens_received": null,
            "cost_usd": null
        }))
        .unwrap();
        assert_eq!(live.tokens_sent, Some(1600));
        assert_eq!(live.tokens_received, None);
        assert_eq!(live.cost_usd, None);
    }

    /// An unknown context window (DESIGN.md: "with no window known,
    /// osi shows context 142 kt with no bar") parses as a present
    /// `context_tokens` and an absent `context_window`.
    #[test]
    fn an_unknown_context_window_parses_as_none() {
        let live = AgentLive::from_value(&json!({"context_tokens": 142000})).unwrap();
        assert_eq!(live.context_tokens, Some(142_000));
        assert_eq!(live.context_window, None);
    }

    #[test]
    fn earlier_attempts_is_present_from_the_second_attempt_on() {
        let live = AgentLive::from_value(&json!({
            "attempt": 2,
            "earlier_attempts": {"tokens_sent": 500, "tokens_received": 40}
        }))
        .unwrap();
        assert_eq!(live.attempt, Some(2));
        assert_eq!(
            live.earlier_attempts,
            Some(AgentLiveEarlierAttempts {
                tokens_sent: Some(500),
                tokens_received: Some(40)
            })
        );
    }

    #[test]
    fn is_recent_is_true_within_5_seconds_and_false_beyond_it() {
        let live = AgentLive {
            updated_at: Some("2026-10-08T11:52:50Z".to_string()),
            ..Default::default()
        };
        assert!(live.is_recent("2026-10-08T11:52:55Z"));
        assert!(!live.is_recent("2026-10-08T11:53:01Z"));
    }

    /// Issue #637's own interrupted-run rule: a leftover `meta.live`
    /// with no further heartbeat reads as stopped, never as running,
    /// once enough time has passed -- `is_recent` alone is what a
    /// front end checks, with no need to know anything about the
    /// run's own process state.
    #[test]
    fn an_interrupted_runs_stale_live_summary_is_not_recent() {
        let live = AgentLive {
            updated_at: Some("2026-10-08T11:46:43Z".to_string()),
            ..Default::default()
        };
        assert!(!live.is_recent("2026-10-08T11:52:55Z"));
    }

    #[test]
    fn is_recent_is_false_with_no_updated_at_at_all() {
        let live = AgentLive::default();
        assert!(!live.is_recent("2026-10-08T11:52:55Z"));
    }

    #[test]
    fn a_non_object_meta_live_value_parses_as_none() {
        assert_eq!(AgentLive::from_value(&Value::Null), None);
        assert_eq!(AgentLive::from_value(&json!([1, 2])), None);
    }

    /// Table-driven: every branch of `run_status` (DESIGN.md §2.1
    /// rule 7), the function that labels a
    /// cancelled run `failed` and an invalid document `aborted`.
    #[test]
    fn run_status_follows_design_rule_7() {
        let ended_ok = json!({
            "runtime": {"last_run": {"completed_at": "t"}},
            "prime": {"meta": {"error": null}}
        });
        let ended_cancelled_sigint = json!({
            "runtime": {"last_run": {"completed_at": "t"}},
            "prime": {"meta": {"error": "Interrupted (Ctrl-C/SIGINT)"}}
        });
        let ended_cancelled_sigterm = json!({
            "runtime": {"last_run": {"completed_at": "t"}},
            "prime": {"meta": {"error": "Interrupted (SIGTERM)"}}
        });
        let ended_failed = json!({
            "runtime": {"last_run": {"completed_at": "t"}},
            "prime": {"meta": {"error": "/bin/ls failed (exit 1): ls: no"}}
        });
        let ended_failed_no_prime = json!({"runtime": {"last_run": {"completed_at": "t"}}});
        let not_ended = json!({"prime": {"meta": {"completed_at": null, "error": null}}});

        struct Case<'a> {
            name: &'static str,
            state: Option<&'a Value>,
            running: bool,
            likely_failed: bool,
            has_failure_reason: bool,
            want: RunStatus,
        }
        let cases = [
            Case {
                name: "running, regardless of state",
                state: None,
                running: true,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Running,
            },
            Case {
                name: "ended ok",
                state: Some(&ended_ok),
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Ok,
            },
            Case {
                name: "ended, error starts with Interrupted (SIGINT)",
                state: Some(&ended_cancelled_sigint),
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Cancelled,
            },
            Case {
                name: "ended, error starts with Interrupted (SIGTERM)",
                state: Some(&ended_cancelled_sigterm),
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Cancelled,
            },
            Case {
                name: "ended, error does not start with Interrupted",
                state: Some(&ended_failed),
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Failed,
            },
            Case {
                name: "ended, no prime node at all (K6)",
                state: Some(&ended_failed_no_prime),
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Failed,
            },
            Case {
                name: "not ended, likely_failed and a failure reason (invalid document, exit 1)",
                state: None,
                running: false,
                likely_failed: true,
                has_failure_reason: true,
                want: RunStatus::Failed,
            },
            Case {
                name: "not ended, likely_failed but no failure reason",
                state: None,
                running: false,
                likely_failed: true,
                has_failure_reason: false,
                want: RunStatus::Aborted,
            },
            Case {
                name: "not ended, a failure reason but not likely_failed (second signal, stray stderr line)",
                state: None,
                running: false,
                likely_failed: false,
                has_failure_reason: true,
                want: RunStatus::Aborted,
            },
            Case {
                name: "not ended, neither likely_failed nor a failure reason",
                state: None,
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Aborted,
            },
            Case {
                name: "state present but not yet ended, no failure signal",
                state: Some(&not_ended),
                running: false,
                likely_failed: false,
                has_failure_reason: false,
                want: RunStatus::Aborted,
            },
        ];

        for case in cases {
            let process = if case.running {
                ProcessState::Running
            } else {
                ProcessState::Exited {
                    interrupted: false,
                    likely_failed: case.likely_failed,
                    has_failure_reason: case.has_failure_reason,
                    exited_at: None,
                }
            };
            assert_eq!(
                run_status(case.state, process),
                case.want,
                "case {:?}",
                case.name
            );
        }
    }

    #[test]
    fn dispatch_event_is_recorded_and_retrievable() {
        let mut model = RunModel::new();
        assert!(model.dispatch_info("prime.each_tree").is_none());
        model.observe_event(&Event::Dispatch {
            ts: "t".to_string(),
            path: "prime.each_tree".to_string(),
            branches: 3,
            concurrency: Some(2),
        });
        assert_eq!(
            model.dispatch_info("prime.each_tree"),
            Some(DispatchInfo {
                branches: 3,
                concurrency: Some(2)
            })
        );
    }

    /// A path's own `start`/`end` events are recorded and
    /// retrievable even though nothing ever reads state for it --
    /// the fallback `render::build` needs for a row with no node of
    /// its own.
    #[test]
    fn event_started_at_and_duration_are_recorded_and_retrievable() {
        let mut model = RunModel::new();
        assert_eq!(model.event_started_at("prime.fan.iter_0.work"), None);
        assert_eq!(model.event_duration_s("prime.fan.iter_0.work"), None);

        model.observe_event(&Event::Start {
            ts: "2026-01-01T11:00:00Z".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.work".to_string(),
        });
        assert_eq!(
            model.event_started_at("prime.fan.iter_0.work"),
            Some("2026-01-01T11:00:00Z")
        );
        // Still open -- no duration until the matching `end` arrives.
        assert_eq!(model.event_duration_s("prime.fan.iter_0.work"), None);

        model.observe_event(&Event::End {
            ts: "2026-01-01T11:00:00.3Z".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.work".to_string(),
            ok: true,
            ms: Some(300),
            error: None,
        });
        assert_eq!(model.event_duration_s("prime.fan.iter_0.work"), Some(0.3));

        // A fresh `start` (a retry) clears the stale duration again.
        model.observe_event(&Event::Start {
            ts: "2026-01-01T11:00:01Z".to_string(),
            id: Some(2),
            path: "prime.fan.iter_0.work".to_string(),
        });
        assert_eq!(model.event_duration_s("prime.fan.iter_0.work"), None);
        assert_eq!(
            model.event_started_at("prime.fan.iter_0.work"),
            Some("2026-01-01T11:00:01Z")
        );
    }

    #[test]
    fn running_or_queued_bound_prefers_concurrency_over_branches() {
        let mut model = RunModel::new();
        assert_eq!(model.running_or_queued_bound("prime.fan"), None);
        model.observe_event(&Event::Dispatch {
            ts: "t".to_string(),
            path: "prime.fan".to_string(),
            branches: 5,
            concurrency: None,
        });
        assert_eq!(model.running_or_queued_bound("prime.fan"), Some(5));
        model.observe_event(&Event::Dispatch {
            ts: "t".to_string(),
            path: "prime.fan".to_string(),
            branches: 5,
            concurrency: Some(2),
        });
        assert_eq!(model.running_or_queued_bound("prime.fan"), Some(2));
    }

    #[test]
    fn an_unfinished_child_of_a_running_tree_container_is_running_or_queued() {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region, ToolOp};

        let root_path = EffectPath::root();
        let fan_path = root_path.push_name("fan");
        let branch_a = fan_path.push_name("a");
        let branch_b = fan_path.push_name("b");
        let tool = |path: EffectPath, name: &str| Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "shell".to_string(),
                params: electricity_bytecode::ParamNode::Literal(electricity_value::Value::None),
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
        };
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: fan_path,
                        name: Some("fan".to_string()),
                        kind: NodeKind::Control(Region::Parallel {
                            branches: vec![tool(branch_a, "a"), tool(branch_b, "b")],
                            max_concurrency: None,
                            stop_on_error: false,
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
        };
        let plan = crate::plan::PlanTree::from_program(&program);

        let mut model = RunModel::new();
        // "a" finished; "fan" itself is still running; "b" never
        // appears (tree branches are invisible while running, §1.4).
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "tree"},
            "fan": {"value": null, "meta": {"completed_at": null},
                "a": {"value": "", "meta": {"completed_at": "t1", "error": null}}
            }
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.fan.b"].kind, StatusKind::RunningOrQueued);
        assert_eq!(rows["prime.fan.a"].kind, StatusKind::Done);
    }

    /// #610: an unnamed report shares its own path with its enclosing
    /// container (here, a named `if`) -- the `--events` fallback for
    /// a report with no plan entry must not also swallow that
    /// container's own real row just because it has seen a report
    /// event at the exact same path.
    #[test]
    fn a_named_ifs_own_row_survives_an_unnamed_report_sharing_its_path() {
        use electricity_bytecode::{
            Condition, EffectPath, LeafKind, NodeKind, OnError, Op, Region, ReportLevel, ReportOp,
            TemplateText,
        };

        let root_path = EffectPath::root();
        let if_path = root_path.clone().push_name("gate");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: if_path.clone(),
                        name: Some("gate".to_string()),
                        kind: NodeKind::Control(Region::If {
                            cond: Condition::Cel {
                                expr: "true".to_string(),
                                strict: false,
                            },
                            then_: Box::new(Region::Block {
                                ops: vec![],
                                overlay: true,
                            }),
                            else_: Some(Box::new(Region::Block {
                                // An unnamed report writes into its
                                // enclosing scope's own path -- here,
                                // the named `if`'s own "prime.gate".
                                ops: vec![Op {
                                    path: if_path.clone(),
                                    name: None,
                                    kind: NodeKind::Leaf(Box::new(LeafKind::Report(ReportOp {
                                        message: TemplateText::new(
                                            "nothing to do",
                                            true,
                                            electricity_bytecode::Escape::None,
                                        ),
                                        data: None,
                                        level: ReportLevel::Info,
                                        description: None,
                                    }))),
                                    on_error: OnError::Fail,
                                    labels: None,
                                    enabled: true,
                                }],
                                overlay: true,
                            })),
                            threshold: 0.5,
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
        };
        let plan = PlanTree::from_program(&program);

        let mut model = RunModel::new();
        model.observe_event(&Event::Report {
            ts: "t0".to_string(),
            path: "prime.gate".to_string(),
            level: crate::observe::ReportLevel::Info,
            message: "nothing to do".to_string(),
            data: None,
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t0"},
            "gate": {"value": null, "meta": {"completed_at": "t0", "branch": "else"}}
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert!(
            rows.contains_key("prime.gate"),
            "the named if's own row must survive an unnamed report sharing its path: {rows:?}"
        );
    }

    #[test]
    fn flattens_nested_nodes_and_skips_last_alias() {
        let state = json!({
            "prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                "each": {"value": null, "meta": {"completed_at": null},
                    "iter_0": {"nap": {"value": "", "meta": {"completed_at": "t1"}}},
                    "last": {"$ref": "iter_0"}
                }
            }
        });
        let flat = flatten_state(&state);
        assert!(flat.contains_key("prime"));
        assert!(flat.contains_key("prime.each"));
        assert!(flat.contains_key("prime.each.iter_0.nap"));
        assert!(!flat.keys().any(|k| k.contains("last")));
    }

    #[test]
    fn a_real_effect_named_last_is_not_mistaken_for_the_ref_alias() {
        // F13: only the exact `{"$ref": "iter_N"}` shape is the loop
        // alias; an ordinary effect an author happened to name `last`
        // must still be walked.
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "last": {"value": "", "meta": {"completed_at": "t1", "error": null}}
            }
        });
        let flat = flatten_state(&state);
        assert!(flat.contains_key("prime.last"));
    }

    #[test]
    fn run_ended_and_ok_from_runtime_and_prime_meta() {
        let ok = json!({"runtime": {"last_run": {"completed_at": "t"}}, "prime": {"meta": {"error": null}}});
        assert!(run_ended(&ok));
        assert!(run_ok(&ok));

        let failed = json!({"runtime": {"last_run": {"completed_at": "t"}}, "prime": {"meta": {"error": "boom"}}});
        assert!(!run_ok(&failed));

        let mid_run = json!({"runtime": {"last_run": {"completed_at": null}}});
        assert!(!run_ended(&mid_run));
    }

    #[test]
    fn run_ok_is_false_when_the_run_ended_with_no_prime_node_at_all() {
        // K6: a document invalid enough that nothing ever ran still
        // gets runtime.last_run.completed_at set in its final write,
        // with no `prime` key whatsoever -- `run_ok` must not treat a
        // missing `/prime/meta/error` pointer the same as an explicit
        // `null`, or this prints `■ run ok` right before the engine's
        // own nonzero exit code.
        let no_prime_at_all = json!({"runtime": {"last_run": {"completed_at": "t"}}});
        assert!(run_ended(&no_prime_at_all));
        assert!(!run_ok(&no_prime_at_all));
    }

    #[test]
    fn done_and_failed_from_completed_node() {
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": true, "meta": {"completed_at": "t", "error": null},
            "ok_leaf": {"value": "x", "meta": {"completed_at": "t", "error": null}},
            "bad_leaf": {"value": null, "meta": {"completed_at": "t", "error": "boom"}}
        }});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::Running);
        assert_eq!(rows["prime.ok_leaf"].kind, StatusKind::Done);
        assert_eq!(rows["prime.bad_leaf"].kind, StatusKind::Failed);
    }

    #[test]
    fn running_node_becomes_cancelled_or_aborted_on_exit() {
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "slow": {"value": null, "meta": {"completed_at": null}}
        }});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::exited(true));
        assert_eq!(rows["prime.slow"].kind, StatusKind::Cancelled);

        let mut model2 = RunModel::new();
        let rows2 = model2.observe(&state, &PlanTree::empty(), ProcessState::exited(false));
        assert_eq!(rows2["prime.slow"].kind, StatusKind::Aborted);
    }

    #[test]
    fn retry_detected_from_moving_created_at() {
        let mut model = RunModel::new();
        let first = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "tool": {"value": null, "meta": {"created_at": "a", "completed_at": null}}
        }});
        let rows = model.observe(&first, &PlanTree::empty(), ProcessState::Running);
        assert!(!rows["prime.tool"].retrying);

        let second = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "tool": {"value": null, "meta": {"created_at": "b", "completed_at": null}}
        }});
        let rows2 = model.observe(&second, &PlanTree::empty(), ProcessState::Running);
        assert!(rows2["prime.tool"].retrying);
    }

    #[test]
    fn retry_detected_from_a_same_created_at_restart_after_a_failure() {
        // #421: created_at no longer moves between a pass's own
        // attempts, so the signal is now completed_at going back to
        // running (null) with created_at unchanged, not created_at
        // itself moving (the case above, kept for whichever
        // engine/version still moves it).
        let mut model = RunModel::new();
        let first = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "tool": {"value": null, "meta": {"created_at": "a", "completed_at": null}}
        }});
        model.observe(&first, &PlanTree::empty(), ProcessState::Running);

        let failed = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "tool": {"value": null, "meta": {"created_at": "a", "completed_at": "b", "error": "boom"}}
        }});
        let rows = model.observe(&failed, &PlanTree::empty(), ProcessState::Running);
        assert!(!rows["prime.tool"].retrying);

        let retrying = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "tool": {"value": null, "meta": {"created_at": "a", "completed_at": null}}
        }});
        let rows2 = model.observe(&retrying, &PlanTree::empty(), ProcessState::Running);
        assert!(rows2["prime.tool"].retrying);
    }

    fn three_step_chain_program() -> electricity_bytecode::Program {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region, ToolOp};

        let root_path = EffectPath::root();
        let tool = |name: &str| Op {
            path: root_path.clone().push_name(name),
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "shell".to_string(),
                params: electricity_bytecode::ParamNode::Literal(electricity_value::Value::None),
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
        };
        electricity_bytecode::Program {
            root: Op {
                path: root_path.clone(),
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![tool("step1"), tool("step2"), tool("step3")],
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
    fn a_chain_sibling_still_running_leaves_the_next_one_pending_not_likely_running() {
        // F8: rule 1 is "every *earlier* plan sibling is complete", not
        // just "the container is running" — step1 hasn't finished, so
        // step2 cannot plausibly have started yet either.
        let plan = PlanTree::from_program(&three_step_chain_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.step2"].kind, StatusKind::Pending);
        assert_eq!(rows["prime.step3"].kind, StatusKind::Pending);
    }

    #[test]
    fn a_chain_sibling_already_done_lets_the_next_one_be_likely_running() {
        let plan = PlanTree::from_program(&three_step_chain_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "error": null}}
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.step2"].kind, StatusKind::LikelyRunning);
        // step3's own earlier sibling, step2, is still absent/incomplete,
        // so step3 itself stays Pending even though step1 is done.
        assert_eq!(rows["prime.step3"].kind, StatusKind::Pending);
    }

    fn chain_with_an_unnamed_if_else_program() -> electricity_bytecode::Program {
        use electricity_bytecode::{
            Condition, EffectPath, LeafKind, NodeKind, OnError, Op, Region, ToolOp,
        };

        let root_path = EffectPath::root();
        let tool = |path: EffectPath| Op {
            path,
            name: None,
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "shell".to_string(),
                params: electricity_bytecode::ParamNode::Literal(electricity_value::Value::None),
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
        };
        let named = |path: EffectPath, name: &str| {
            let mut op = tool(path);
            op.name = Some(name.to_string());
            op
        };
        electricity_bytecode::Program {
            root: Op {
                path: root_path.clone(),
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![
                        named(root_path.clone().push_name("a"), "a"),
                        Op {
                            path: root_path.clone(),
                            name: None,
                            kind: NodeKind::Control(Region::If {
                                cond: Condition::Cel {
                                    expr: "true".to_string(),
                                    strict: false,
                                },
                                then_: Box::new(Region::Block {
                                    ops: vec![named(
                                        root_path.clone().push_name("then_branch"),
                                        "then_branch",
                                    )],
                                    overlay: true,
                                }),
                                else_: Some(Box::new(Region::Block {
                                    ops: vec![named(
                                        root_path.clone().push_name("else_branch"),
                                        "else_branch",
                                    )],
                                    overlay: true,
                                })),
                                threshold: 0.5,
                            }),
                            on_error: OnError::Fail,
                            labels: None,
                            enabled: true,
                        },
                        named(root_path.push_name("b"), "b"),
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
        }
    }

    #[test]
    fn an_untaken_if_else_branch_never_blocks_the_chain_sibling_that_follows_it() {
        // Review finding 9: once the taken branch (`else_branch` here)
        // is actually observed, the untaken one (`then_branch`) must
        // resolve to something `sibling_is_complete` counts as settled
        // — not get stuck forever at `LikelyRunning`, which would also
        // keep `b`, after the `if`, Pending forever.
        let plan = PlanTree::from_program(&chain_with_an_unnamed_if_else_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "error": null}},
            "else_branch": {"value": "", "meta": {"created_at": "t1", "completed_at": "t2", "error": null}}
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.then_branch"].kind, StatusKind::Skipped);
        assert_eq!(rows["prime.b"].kind, StatusKind::LikelyRunning);
    }

    #[test]
    fn with_neither_if_else_branch_observed_yet_the_chain_sibling_after_stays_pending() {
        // Before either side has run, both are equally plausible, and
        // DESIGN.md §2.1 rule 1 still requires every earlier sibling —
        // the whole `if`, here — to be complete before `b` can even be
        // a *guess* at running.
        let plan = PlanTree::from_program(&chain_with_an_unnamed_if_else_program());
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "error": null}}
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.b"].kind, StatusKind::Pending);
    }

    #[test]
    fn a_named_loops_pass_template_is_never_seeded_as_a_stale_pending_row() {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopId, LoopSpec, NodeKind, OnError, Op, Region, ToolOp,
        };

        let root_path = EffectPath::root();
        let loop_path = root_path.push_name("each_chain");
        let pass_path = loop_path.push_pass(LoopId(0));
        let body_path = pass_path.push_name("nap");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: loop_path,
                        name: Some("each_chain".to_string()),
                        kind: NodeKind::Control(Region::Loop {
                            name: Some("each_chain".to_string()),
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
                                overlay: true,
                            }),
                            flow: electricity_bytecode::LoopFlow::Chain,
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
        };
        let plan = PlanTree::from_program(&program);
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "each_chain": {"value": null, "meta": {"completed_at": null},
                "iter_0": {"nap": {"value": "", "meta": {"completed_at": "t1", "error": null}}}
            }
        }});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.each_chain.iter_0.nap"].kind, StatusKind::Done);
        assert!(
            !rows.keys().any(|k| k.contains("iter_*")),
            "a pass-template row should never be seeded: {:?}",
            rows.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_open_start_is_running_even_absent_from_state() {
        // A tree branch is never visible in state while it runs
        // (DESIGN.md §1.4); events are the only way to see it at all.
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.fan.a".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::Running);
        assert_eq!(rows["prime.fan.a"].kind, StatusKind::Running);
    }

    #[test]
    fn an_end_with_ok_is_done_and_an_end_with_error_is_failed() {
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "t1".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
            ok: true,
            ms: Some(5),
            error: None,
        });
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(2),
            path: "prime.b".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "t1".to_string(),
            id: Some(2),
            path: "prime.b".to_string(),
            ok: false,
            ms: Some(5),
            error: Some("boom".to_string()),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::Running);
        assert_eq!(rows["prime.a"].kind, StatusKind::Done);
        assert_eq!(rows["prime.b"].kind, StatusKind::Failed);
    }

    #[test]
    fn an_end_with_error_and_on_error_skip_in_the_plan_is_failed_handled() {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region, ToolOp};

        let root_path = EffectPath::root();
        let child_path = root_path.push_name("flaky");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: child_path,
                        name: Some("flaky".to_string()),
                        kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
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
                        }))),
                        on_error: OnError::Skip,
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
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.flaky".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "t1".to_string(),
            id: Some(1),
            path: "prime.flaky".to_string(),
            ok: false,
            ms: Some(5),
            error: Some("boom".to_string()),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &plan, ProcessState::Running);
        assert_eq!(rows["prime.flaky"].kind, StatusKind::FailedHandled);
    }

    #[test]
    fn an_open_start_is_cancelled_when_run_end_arrives_interrupted() {
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.slow".to_string(),
        });
        model.observe_event(&Event::RunEnd {
            ts: "t1".to_string(),
            ok: false,
            error: Some("Interrupted (Ctrl-C/SIGINT)".to_string()),
            signal: Some("SIGINT".to_string()),
        });
        assert!(model.run_ended_by_events());
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::exited(true));
        assert_eq!(rows["prime.slow"].kind, StatusKind::Cancelled);
    }

    #[test]
    fn an_open_start_is_aborted_when_the_process_exits_with_no_run_end() {
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.slow".to_string(),
        });
        assert!(!model.run_ended_by_events());
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::exited(false));
        assert_eq!(rows["prime.slow"].kind, StatusKind::Aborted);
    }

    #[test]
    fn run_start_pid_and_id_are_recorded() {
        let mut model = RunModel::new();
        assert_eq!(model.run_start_pid(), None);
        model.observe_event(&Event::RunStart {
            ts: "t0".to_string(),
            run_id: "abc".to_string(),
            pid: Some(4242),
            engine: None,
        });
        assert_eq!(model.run_start_pid(), Some(4242));
        assert_eq!(model.run_start_id(), Some("abc"));
    }

    #[test]
    fn a_retry_reopens_a_path_events_already_marked_ended() {
        // A path's `end` must not keep answering for it forever once a
        // fresh `start` reuses the same path (a retry, or the next
        // tree-flow instance reusing an unnamed path, DESIGN.md §2.3).
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.flaky".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "t1".to_string(),
            id: Some(1),
            path: "prime.flaky".to_string(),
            ok: false,
            ms: Some(5),
            error: Some("boom".to_string()),
        });
        model.observe_event(&Event::Start {
            ts: "t2".to_string(),
            id: Some(2),
            path: "prime.flaky".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::Running);
        assert_eq!(rows["prime.flaky"].kind, StatusKind::Running);
    }

    #[test]
    fn an_end_with_id_null_closes_the_oldest_open_idd_start_at_its_path() {
        // Issue #433: an `end` naming no `id` of its own can still
        // close a `start` that had one, at the same path -- without
        // this, the id'd `start` stays open forever, and the row ends
        // the run looking cancelled/aborted instead of however it
        // really ended.
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(7),
            path: "prime.flaky".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "t1".to_string(),
            id: None,
            path: "prime.flaky".to_string(),
            ok: true,
            ms: Some(5),
            error: None,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rows = model.observe(&state, &PlanTree::empty(), ProcessState::exited(true));
        assert_eq!(rows["prime.flaky"].kind, StatusKind::Done);
    }

    #[test]
    fn an_end_with_id_null_closes_the_oldest_of_several_open_idd_starts_at_its_path() {
        // Several concurrent instances at the same unnamed path
        // (tree-flow passes reusing it, DESIGN.md §2.3), each with its
        // own id: an `end` with no id of its own must close the
        // oldest still-open one, not an arbitrary one or the newest.
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "t0".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&Event::Start {
            ts: "t1".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "t2".to_string(),
            id: None,
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(5),
            error: None,
        });
        assert!(model.open_by_id.values().any(|p| p == "prime.u_nap"));
        assert_eq!(
            model.open_by_id.get(&2).map(String::as_str),
            Some("prime.u_nap"),
            "the oldest id (1), not the newest (2), should have closed"
        );
    }

    /// The token rule (issue #527, finding F4): a
    /// retried prompt where the two candidate pairs actually
    /// *disagree*, so the test can tell them apart — unlike a
    /// retryable adapter error (which carries no tokens of its own),
    /// a `prompt_type: json` reply that fails to *decode* still
    /// spends real tokens on that attempt (`core/prompt.py`'s
    /// `_generate_with_fallbacks`: a decode failure is retryable, and
    /// every attempt's tokens accumulate into `_total` regardless of
    /// whether it won). This state is `cof run`'s and electricity's
    /// own real final state (not hand-written) for the identical
    /// scenario: a first reply of invalid JSON with `tokens_sent: 5`/
    /// `tokens_received: 7`, then a valid one with `11`/`22` —
    /// confirmed by running both engines with the scripted adapter.
    /// Both wrote the identical `tokens_sent_total: 16`/
    /// `tokens_received_total: 29` and the identical
    /// `runtime.last_run.totals` from it — the plain pair
    /// (`tokens_sent: 11`/`tokens_received: 22`, the winning attempt
    /// only) does not match those totals, so a rule that read the
    /// plain pair instead would fail this test.
    #[test]
    fn subtree_tokens_matches_run_totals_for_a_retried_prompt_on_cof() {
        let state = json!({
            "runtime": {"last_run": {"totals": {"tokens_sent": 16, "tokens_received": 29}}},
            "prime": {"value": true, "meta": {
                "completed_at": "t1", "error": null, "tokens_sent": null, "tokens_received": null
            },
            "ask": {"value": {"ans": "ok"}, "meta": {
                "created_at": "t0", "completed_at": "t1", "error": null,
                "prompt_type": "json",
                "retries_used": 1,
                "tokens_sent": 11, "tokens_received": 22,
                "tokens_sent_total": 16, "tokens_received_total": 29,
                "fallback_attempts": [
                    {"adapter": "scripted", "model": "scripted-model", "status": "succeeded",
                     "error": null, "tokens_sent": 11, "tokens_received": 22}
                ]
            }}}
        });
        assert_token_rule_holds(&state);
    }

    /// [`subtree_tokens_matches_run_totals_for_a_retried_prompt_on_cof`],
    /// electricity's own final state for the identical scenario and
    /// identical replies file — the two engines agree byte-for-byte
    /// on both the `_total` pair and `runtime.last_run.totals`.
    #[test]
    fn subtree_tokens_matches_run_totals_for_a_retried_prompt_on_electricity() {
        let state = json!({
            "runtime": {"last_run": {"totals": {"tokens_sent": 16, "tokens_received": 29}}},
            "prime": {"value": true, "meta": {
                "completed_at": "t1", "error": null, "tokens_sent": null, "tokens_received": null
            },
            "ask": {"value": {"ans": "ok"}, "meta": {
                "created_at": "t0", "completed_at": "t1", "error": null,
                "prompt_type": "json",
                "retries_used": 1,
                "tokens_sent": 11, "tokens_received": 22,
                "tokens_sent_total": 16, "tokens_received_total": 29,
                "fallback_attempts": [
                    {"adapter": "scripted", "model": "scripted-model", "status": "succeeded",
                     "error": null, "tokens_sent": 11, "tokens_received": 22}
                ]
            }}}
        });
        assert_token_rule_holds(&state);
    }

    fn assert_token_rule_holds(state: &Value) {
        let flat = flatten_state(state);
        let totals = run_totals(state).unwrap();
        let (sent, received) = subtree_tokens("prime", &flat);
        assert_eq!(sent, totals["tokens_sent"].as_u64().unwrap());
        assert_eq!(received, totals["tokens_received"].as_u64().unwrap());
        assert_eq!((sent, received), (16, 29));
        // The plain pair (the winning attempt only) must *not* match —
        // otherwise this test could not actually tell the two rules
        // apart (finding F4).
        assert_ne!((sent, received), (11, 22));
    }

    /// Issue #650's own seam: an unnamed loop's passes reuse the same
    /// path, but `--events` still gives each pass its own id, so
    /// `step_intervals` logs both passes as separate intervals rather
    /// than overwriting one with the other.
    #[test]
    fn step_intervals_pairs_an_unnamed_loops_passes_by_event_id() {
        let mut model = RunModel::new();
        model.observe_event(&Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(5000),
            error: None,
        });
        model.observe_event(&Event::Start {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&Event::End {
            ts: "2026-01-01T00:00:12Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(7000),
            error: None,
        });
        let intervals = model.step_intervals();
        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].start_ts, "2026-01-01T00:00:00Z");
        assert_eq!(intervals[0].end_ts, "2026-01-01T00:00:05Z");
        assert_eq!(intervals[1].start_ts, "2026-01-01T00:00:05Z");
        assert_eq!(intervals[1].end_ts, "2026-01-01T00:00:12Z");
        assert!(intervals.iter().all(|i| i.path == "prime.u_nap"));
    }

    /// Without `--events`, an unnamed loop's finished pass is only
    /// ever visible through state's own `created_at`/`completed_at`,
    /// captured by `observe`'s own "just finished" hook
    /// (`push_state_interval`) exactly once per `created_at`, even
    /// though the loop reuses the same path on every pass.
    #[test]
    fn step_intervals_from_state_alone_captures_each_passes_own_created_at() {
        let mut model = RunModel::new();
        let first = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "u_nap": {"value": "n0", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:05Z", "error": null}}
        }});
        model.observe(&first, &PlanTree::empty(), ProcessState::Running);
        let second = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "u_nap": {"value": "n1", "meta": {"created_at": "2026-01-01T00:00:05Z", "completed_at": "2026-01-01T00:00:12Z", "error": null}}
        }});
        model.observe(&second, &PlanTree::empty(), ProcessState::Running);
        // Re-observing the same second snapshot again (the TUI's own
        // redraw tick) must not log the same pass twice.
        model.observe(&second, &PlanTree::empty(), ProcessState::Running);

        let intervals = model.step_intervals();
        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].start_ts, "2026-01-01T00:00:00Z");
        assert_eq!(intervals[0].end_ts, "2026-01-01T00:00:05Z");
        assert_eq!(intervals[1].start_ts, "2026-01-01T00:00:05Z");
        assert_eq!(intervals[1].end_ts, "2026-01-01T00:00:12Z");
    }

    /// Past [`MAX_STEP_INTERVALS`], a dropped pass no longer keeps
    /// its own path, only a count per clock minute of its start and
    /// end events (issue #649: "then keeps only counts per minute") --
    /// the Time screen's events/min strip's own tail at this scale.
    #[test]
    fn step_intervals_past_the_cap_count_by_minute_not_by_path() {
        let mut model = RunModel::new();
        for i in 0..(MAX_STEP_INTERVALS + 3) {
            let id = i as i64 + 1;
            model.observe_event(&Event::Start {
                ts: "2026-01-01T00:00:00Z".to_string(),
                id: Some(id),
                path: "prime.loop.nap".to_string(),
            });
            model.observe_event(&Event::End {
                ts: "2026-01-01T00:01:00Z".to_string(),
                id: Some(id),
                path: "prime.loop.nap".to_string(),
                ok: true,
                ms: Some(60_000),
                error: None,
            });
        }
        assert_eq!(model.step_intervals().len(), MAX_STEP_INTERVALS);
        assert_eq!(model.overflowed_total_passes(), 3);
        let minutes = model.overflowed_minute_counts();
        // Every dropped pass starts at :00:00 and ends at :01:00 --
        // one bucket per minute, 3 events each (3 dropped passes).
        assert_eq!(minutes.get("2026-01-01T00:00"), Some(&3));
        assert_eq!(minutes.get("2026-01-01T00:01"), Some(&3));
    }
}
