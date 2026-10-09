//! `RunModel`: the rows `osp` renders, their status (DESIGN.md §2),
//! loop progress, run totals, and `$ref`/`last` alias handling. O-1
//! work.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::observe::Event;
use crate::plan::{Flow, PlanTree};

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
/// skipped — osp always resolves the real `iter_N` node instead.
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
        // skip it so osp always resolves the real `iter_N` node
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

pub fn run_totals(state: &Value) -> Option<Value> {
    state.pointer("/runtime/last_run/totals").cloned()
}

/// Why a plan node was skipped (DESIGN.md §2.2). `Unknown` covers every
/// node osp infers as skipped without a more specific reason (e.g. one
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    /// The process exited; `interrupted` is `prime.meta.error` starting
    /// with `"Interrupted"` (distinguishing cancelled from aborted).
    Exited {
        interrupted: bool,
    },
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
    cancelled_by_run_end: BTreeSet<String>,
    run_start: Option<(String, Option<i32>)>,
    run_end: Option<(bool, Option<String>)>,
}

impl RunModel {
    pub fn new() -> Self {
        RunModel {
            last_created_at: BTreeMap::new(),
            dispatch: BTreeMap::new(),
            open_by_id: BTreeMap::new(),
            open_unid: BTreeMap::new(),
            ended: BTreeMap::new(),
            cancelled_by_run_end: BTreeSet::new(),
            run_start: None,
            run_end: None,
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
            Event::RunStart { run_id, pid, .. } => {
                self.run_start = Some((run_id.clone(), *pid));
            }
            Event::Start { id, path, .. } => {
                // A fresh `start` at a path this instance already saw
                // `end` for is a retry (or the next tree-flow instance
                // reusing an unnamed path, DESIGN.md §2.3): either way
                // it is open again now, so a stale `ended` entry must
                // not keep answering for it.
                self.ended.remove(path);
                match id {
                    Some(id) => {
                        self.open_by_id.insert(*id, path.clone());
                    }
                    None => {
                        *self.open_unid.entry(path.clone()).or_insert(0) += 1;
                    }
                }
            }
            Event::End {
                id,
                path,
                ok,
                error,
                ..
            } => {
                match id {
                    Some(id) => {
                        self.open_by_id.remove(id);
                    }
                    None => {
                        if let Some(count) = self.open_unid.get_mut(path) {
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
            Event::RunEnd { ok, error, .. } => {
                self.run_end = Some((*ok, error.clone()));
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
        }
    }

    /// Whether a `run_end` line has arrived yet (DESIGN.md §3: when it
    /// has, the final live-state write is already on disk) — `osp
    /// watch`'s own stop condition alongside a completed state (F4).
    pub fn run_ended_by_events(&self) -> bool {
        self.run_end.is_some()
    }

    /// `run_end`'s own `ok`, when one has arrived — lets a caller settle
    /// a completed run's exit status from the event itself rather than
    /// re-deriving it from state (F4).
    pub fn run_end_ok(&self) -> Option<bool> {
        self.run_end.as_ref().map(|(ok, _)| *ok)
    }

    /// The engine's own pid, from `run_start` (DESIGN.md §3's format
    /// table), when an events stream carried one — lets `osp watch`
    /// tell a genuine abort (no `run_end`, dead process) from a run
    /// simply still going, with no child handle of its own (F4).
    pub fn run_start_pid(&self) -> Option<i32> {
        self.run_start.as_ref().and_then(|(_, pid)| *pid)
    }

    /// The run id a `run_start` event announced, when one arrived.
    pub fn run_start_id(&self) -> Option<&str> {
        self.run_start.as_ref().map(|(run_id, _)| run_id.as_str())
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
            let retrying = if node.is_running() {
                match (self.last_created_at.get(path), &node.created_at) {
                    (Some(prev), Some(now)) => prev != now,
                    _ => false,
                }
            } else {
                false
            };
            if let Some(created_at) = &node.created_at {
                self.last_created_at
                    .insert(path.clone(), created_at.clone());
            }

            let kind = if node.is_running() {
                match process {
                    ProcessState::Running => StatusKind::Running,
                    ProcessState::Exited { interrupted: true } => StatusKind::Cancelled,
                    ProcessState::Exited { interrupted: false } => StatusKind::Aborted,
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
                Some(EventOutcome::Open) => match process {
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

/// Status of the run as a whole (DESIGN.md §2.1 rule 7).
pub fn run_status(state: Option<&Value>, process: ProcessState) -> RunStatus {
    match process {
        ProcessState::Running => RunStatus::Running,
        ProcessState::Exited { interrupted } => {
            let Some(state) = state else {
                return if interrupted {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Aborted
                };
            };
            if !run_ended(state) {
                return if interrupted {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Aborted
                };
            }
            if run_ok(state) {
                RunStatus::Ok
            } else {
                RunStatus::Failed
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
        let rows = model.observe(
            &state,
            &PlanTree::empty(),
            ProcessState::Exited { interrupted: true },
        );
        assert_eq!(rows["prime.slow"].kind, StatusKind::Cancelled);

        let mut model2 = RunModel::new();
        let rows2 = model2.observe(
            &state,
            &PlanTree::empty(),
            ProcessState::Exited { interrupted: false },
        );
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
        let rows = model.observe(
            &state,
            &PlanTree::empty(),
            ProcessState::Exited { interrupted: true },
        );
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
        let rows = model.observe(
            &state,
            &PlanTree::empty(),
            ProcessState::Exited { interrupted: false },
        );
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
}
