//! `RunModel`: the rows `osp` renders, their status (DESIGN.md §2),
//! loop progress, run totals, and `$ref`/`last` alias handling. O-1
//! work.

use std::collections::BTreeMap;

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
        if key == "value" || key == "meta" || key == "last" {
            continue;
        }
        let child_path = format!("{path}.{key}");
        walk(child, &child_path, out);
    }
}

/// `runtime.last_run.completed_at` is set — the run has ended, whether
/// cleanly or not (DESIGN.md §2.1 rule 7).
pub fn run_ended(state: &Value) -> bool {
    state
        .pointer("/runtime/last_run/completed_at")
        .is_some_and(|v| !v.is_null())
}

/// `prime.meta.error == null` (DESIGN.md §2.1 rule 7).
pub fn run_ok(state: &Value) -> bool {
    state
        .pointer("/prime/meta/error")
        .is_none_or(Value::is_null)
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

/// Tracks per-path status across a run's observations, from state alone
/// (DESIGN.md §2.1's "From state alone (best effort)" rules) plus
/// whatever `--events` dispatch info has arrived (#419): a stream from
/// a `cof` without `--events` simply never calls `observe_event`, so
/// every status still comes from `observe`'s own state-only rules.
pub struct RunModel {
    last_created_at: BTreeMap<String, String>,
    dispatch: BTreeMap<String, DispatchInfo>,
}

impl RunModel {
    pub fn new() -> Self {
        RunModel {
            last_created_at: BTreeMap::new(),
            dispatch: BTreeMap::new(),
        }
    }

    /// Feeds one parsed `--events` line in. Only `Dispatch` is tracked
    /// today (DESIGN.md §2.1 rule 4's exact bound); the other variants
    /// are reserved for the events-based exact-status rules once a plan
    /// and an event stream are both available end to end.
    pub fn observe_event(&mut self, event: &Event) {
        if let Event::Dispatch {
            path,
            branches,
            concurrency,
            ..
        } = event
        {
            self.dispatch.insert(
                path.clone(),
                DispatchInfo {
                    branches: *branches,
                    concurrency: *concurrency,
                },
            );
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
                match plan.match_path(path).and_then(|m| m.entries.first()) {
                    Some(entry)
                        if matches!(
                            entry.on_error,
                            electricity_bytecode::OnError::Skip
                                | electricity_bytecode::OnError::Continue
                        ) =>
                    {
                        StatusKind::FailedHandled
                    }
                    _ => StatusKind::Failed,
                }
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
                rows.insert(path.clone(), self.infer_absent(path, &flat, plan));
            }
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
        // container is still running (or is the root itself), so this
        // node may simply not have been written yet.
        if entry.parent_flow == Some(Flow::Chain) {
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
}
