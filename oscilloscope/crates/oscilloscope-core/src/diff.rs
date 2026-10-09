//! Turns a live-state snapshot diff, or an `--events` line, into log
//! lines (DESIGN.md §2.4), sorted by `meta.created_at`/`completed_at`
//! rather than observation order.

use std::collections::{BTreeMap, BTreeSet};

use electricity_bytecode::OnError;
use serde_json::Value;

use crate::model::{NodeMeta, RunModel, flatten_state, run_ended, run_error, run_ok, run_totals};
use crate::observe::Event;
use crate::plan::PlanTree;

/// One line of the `--log` stream (DESIGN.md §2.4, §6.2). `ts` is the
/// event's own timestamp (`meta.created_at`/`completed_at`, never when
/// osp noticed the change) — `None` only for the final run-summary
/// line when no timestamp is available at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub ts: Option<String>,
    pub text: String,
}

/// Sorts lines from two different observations (a state `diff` and an
/// `--events` line, each already timestamped from its own source) into
/// one chronological stream (DESIGN.md §2: "Log lines are sorted by
/// those times", never by which poll noticed them first). `None` (the
/// final run-summary line, when no timestamp was ever available at
/// all) sorts last.
pub fn sort_log_lines(lines: &mut [LogLine]) {
    lines.sort_by(|a, b| match (&a.ts, &b.ts) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// Caps one log entry to its first line and `max` characters (DESIGN.md
/// §2.4: "Values are never logged in full: one line, with a character
/// cap") — a multi-line error message or a tool's multi-line stdout
/// used to flow straight into the log otherwise, breaking the
/// mm:ss.s-per-line shape every other entry keeps (F13).
fn truncate_chars(s: &str, max: usize) -> String {
    let first_line = s.split_once('\n').map_or(s, |(line, _)| line);
    if first_line.chars().count() <= max {
        first_line.to_string()
    } else {
        first_line.chars().take(max).collect()
    }
}

fn summary_for(node: &NodeMeta) -> String {
    let meta = &node.meta;
    if let Some(provider) = meta.get("provider").and_then(Value::as_str) {
        if let Some(rendered) = meta.get("params_rendered") {
            if let Some(command) = rendered.get("command").and_then(Value::as_str) {
                let args: Vec<String> = rendered
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                return format!("{provider} {command} {}", args.join(" "));
            }
        }
        return format!("{provider} tool");
    }
    if let Some(adapter) = meta.get("adapter").and_then(Value::as_str) {
        let model = meta.get("model").and_then(Value::as_str).unwrap_or("?");
        let prompt_sent = meta
            .get("prompt_sent")
            .and_then(Value::as_str)
            .unwrap_or("");
        return format!(
            "prompt {adapter}/{model} \"{}\"",
            truncate_chars(prompt_sent, 60)
        );
    }
    "effect".to_string()
}

fn result_summary(node: &NodeMeta) -> String {
    if let Some(stdout) = node.meta.get("stdout").and_then(Value::as_str) {
        return truncate_chars(stdout.trim_end(), 60);
    }
    if let Some(tokens_sent) = node.meta.get("tokens_sent").and_then(Value::as_u64) {
        let received = node
            .meta
            .get("tokens_received")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return format!("↑{tokens_sent} ↓{received}");
    }
    "ok".to_string()
}

/// A dynamic/`if`/loop container, identified from its own measured
/// `meta` shape (DESIGN.md §1.2) rather than the plan, since the plan
/// may not exist (the no-plan fallback, DESIGN.md §5's "Asks" item 5).
/// Containers get no `▶`/`✓`/`✗` line of their own (DESIGN.md §6.2's
/// example has none for the root `prime`); their children's lines, and
/// a container's own `◆`/`⟳` line, already show its progress.
fn is_container(meta: &Value) -> bool {
    meta.get("flow").is_some() || meta.get("mode").is_some()
}

/// Whether `path` is a container, for an `--events` line that has no
/// `meta` of its own to check (DESIGN.md §1.4: `start`/`end` fire for
/// the root, every *named* container, and every leaf alike — an
/// unnamed `if`/loop is the only thing that never gets one). The
/// document root is always one (checked first, needing neither a plan
/// nor any state at all); the plan answers for everything else when
/// there is one; a `dispatch` event (K5) catches a *tree* container's
/// own `end` even with no plan at all — but, per the real recorded
/// shape (`dispatch` always comes strictly *after* the container's
/// own `start`, since a container must have already started to go on
/// and dispatch its branches), never its *own* `start`. With no plan,
/// the latest state `diff` has already seen for the path is the last
/// resort (same check `diff`'s own container suppression uses) —
/// returning `None` when none of these can say yet, rather than
/// guessing "leaf", is what lets `diff_event`'s `Start` handling defer
/// the decision instead of committing to a wrong line immediately.
fn container_signal(
    path: &str,
    plan: &PlanTree,
    last: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
) -> Option<bool> {
    // The document root is always a container (DESIGN.md §1.3: "root:
    // prime, a node with its own meta"; §6.2's own example has no
    // line for it) — the one case this can say for certain with
    // neither a plan nor any state observed yet, closing the gap the
    // fallback below otherwise has for a root `start` arriving before
    // its first state write (DESIGN.md §1.1: 0.4–0.9s after launch).
    if path == "prime" {
        return Some(true);
    }
    if let Some(m) = plan.match_path(path) {
        if let Some(entry) = m.entries.first() {
            return Some(!matches!(
                entry.kind,
                crate::plan::PlanEntryKind::Leaf | crate::plan::PlanEntryKind::Use
            ));
        }
    }
    if model.dispatch_info(path).is_some() {
        return Some(true);
    }
    last.get(path).map(|n| is_container(&n.meta))
}

/// `container_signal`, with "don't know yet" folded into "not a
/// container" — the right default once an `end` has arrived: by then,
/// a tree container's own `dispatch` has already been seen (the one
/// case `container_signal` can still resolve after `start`), so the
/// only paths left genuinely unresolved are chain containers with
/// neither a plan nor any state write yet, the same narrow gap this
/// whole fallback already had.
fn is_container_path(
    path: &str,
    plan: &PlanTree,
    last: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
) -> bool {
    container_signal(path, plan, last, model).unwrap_or(false)
}

/// Whether any node strictly under `path` already carries its own
/// `meta.error` (P2-6): the signal that a container's own failure is
/// really just a descendant's failure bubbling up, which already got
/// its own ✗ line, rather than something genuinely invisible
/// otherwise (an `each` over a path that never resolved, a CEL error
/// evaluating an `if`'s condition, neither of which ever starts a
/// child at all).
fn any_descendant_failed(path: &str, flat: &BTreeMap<String, NodeMeta>) -> bool {
    let prefix = format!("{path}.");
    flat.iter()
        .any(|(other, node)| other.starts_with(&prefix) && node.error.is_some())
}

fn on_error_suffix(path: &str, plan: &PlanTree) -> &'static str {
    match plan
        .match_path(path)
        .and_then(|m| m.entries.first().map(|e| e.on_error))
    {
        Some(OnError::Continue) => "  (on_error: continue)",
        // `skip` and `continue` give identical nodes (DESIGN.md §1.2);
        // the suffix is what tells the two apart in the log, so `skip`
        // needs its own text, not just "anything but continue is
        // nothing" (F13).
        Some(OnError::Skip) => "  (on_error: skip)",
        _ => "",
    }
}

fn duration_text(created_at: &Option<String>, completed_at: &Option<String>) -> String {
    match (created_at.as_deref(), completed_at.as_deref()) {
        (Some(c0), Some(c1)) => match crate::observe::duration_seconds(c0, c1) {
            Some(secs) => format!("{secs:.1}s"),
            None => "?".to_string(),
        },
        _ => "?".to_string(),
    }
}

/// Tracks each run's own progress across consecutive snapshots, so a
/// transition (a node appearing, completing, or moving) can be told
/// from the previous observation — DESIGN.md §2.4's diff table.
pub struct Differ {
    last: BTreeMap<String, NodeMeta>,
    run_line_emitted: bool,
    /// Counts a reused unnamed-loop path's passes (DESIGN.md §2.3): a
    /// complete node whose `created_at` moves forward is the next pass.
    unnamed_pass_counts: BTreeMap<String, u32>,
    /// Every path `diff_event` has ever reported a line for — `diff`
    /// itself then leaves that path's `▶`/`✓`/`✗`/`↻` lines to it
    /// (F1): events are exact and timestamped at the real start/end,
    /// where a state diff only learns of either at its next poll —
    /// printing both would duplicate the same transition, once from
    /// each source.
    event_sourced: BTreeSet<String>,
    /// Paths with a `start` event and no matching `end` yet (K5):
    /// `diff`'s own `■ run ...` line must never print while one of
    /// these is still open. State and events are two independently
    /// polled files; a tick can observe the state's final write (the
    /// run container itself is done) slightly ahead of the events
    /// tailer catching up on the very last leaf's own `end` —
    /// deferring the summary line until every event-sourced path has
    /// actually closed keeps the last effect's own line from printing
    /// *after* the run's. Cleared by a `run_end` too: an effect
    /// interrupted mid-flight never gets its own `end` at all.
    open_event_paths: BTreeSet<String>,
}

impl Differ {
    pub fn new() -> Self {
        Differ {
            last: BTreeMap::new(),
            run_line_emitted: false,
            unnamed_pass_counts: BTreeMap::new(),
            event_sourced: BTreeSet::new(),
            open_event_paths: BTreeSet::new(),
        }
    }

    /// Turns one parsed `--events` line into its own log line
    /// (DESIGN.md §2.4), immediately — unlike `diff`, this needs no
    /// previous snapshot to compare against, since a `start`/`end` is
    /// itself the transition. The summary/result text still comes from
    /// the latest state `diff` has seen for the path, when there is
    /// any (events carry no values, DESIGN.md §3); a tree branch or
    /// `use` child that never reaches state before it ends falls back
    /// to a generic line rather than one with nothing to show.
    pub fn diff_event(&mut self, event: &Event, plan: &PlanTree, model: &RunModel) -> Vec<LogLine> {
        match event {
            Event::Start { ts, path, .. } => {
                self.event_sourced.insert(path.clone());
                self.open_event_paths.insert(path.clone());
                // A named container (the document root included) fires
                // `start`/`end` the same as a leaf (DESIGN.md §1.4's
                // probe notes), but gets no ▶/✓/✗ of its own — same
                // rule `diff` already applies from state (F1 follow-up,
                // caught once a real plan started compiling and
                // `--events` started firing for more than leaves). A
                // tree container's own `dispatch` always comes
                // strictly *after* its `start` though (it must already
                // be running to dispatch anything), so this can't catch
                // it here — only its later `end` (below).
                if is_container_path(path, plan, &self.last, model) {
                    return Vec::new();
                }
                let summary = self
                    .last
                    .get(path)
                    .map(summary_for)
                    .unwrap_or_else(|| "effect".to_string());
                vec![LogLine {
                    ts: Some(ts.clone()),
                    text: format!("▶ {path}  {summary}"),
                }]
            }
            Event::End {
                ts,
                path,
                ok,
                ms,
                error,
                ..
            } => {
                self.event_sourced.insert(path.clone());
                self.open_event_paths.remove(path);
                if is_container_path(path, plan, &self.last, model) {
                    // P2-6: the container's own failure, when it has
                    // one, still needs a line if nothing under it
                    // already printed one -- the events path has the
                    // exact same gap state's `diff` does.
                    if !*ok && !any_descendant_failed(path, &self.last) {
                        let message = error.as_deref().unwrap_or("");
                        return vec![LogLine {
                            ts: Some(ts.clone()),
                            text: format!("✗ {path}  {}", truncate_chars(message, 120)),
                        }];
                    }
                    return Vec::new();
                }
                let text = if *ok {
                    let duration = ms
                        .map(|m| format!("{:.1}s", m as f64 / 1000.0))
                        .unwrap_or_else(|| "?".to_string());
                    let result = self
                        .last
                        .get(path)
                        .map(result_summary)
                        .unwrap_or_else(|| "ok".to_string());
                    format!("✓ {path}  {duration}  {result}")
                } else {
                    let message = error.as_deref().unwrap_or("");
                    format!(
                        "✗ {path}  {}{}",
                        truncate_chars(message, 120),
                        on_error_suffix(path, plan)
                    )
                };
                vec![LogLine {
                    ts: Some(ts.clone()),
                    text,
                }]
            }
            Event::RunEnd { .. } => {
                // An effect interrupted mid-flight never gets its own
                // `end` at all (DESIGN.md §2.1's cancelled rule) —
                // without this, one open, never-closed path would
                // defer the run summary line forever.
                self.open_event_paths.clear();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Whether a `■ run ...` summary line has already been emitted
    /// (F5): lets a caller fall back to a synthetic summary — from the
    /// final `--out` write `diff` never saw, or from the engine's own
    /// pre-execution JSON error, or "aborted" — only when `diff` itself
    /// never had a completed `prime` to report one from.
    pub fn run_line_emitted(&self) -> bool {
        self.run_line_emitted
    }

    /// Diffs `state` against the previous snapshot this instance was
    /// given, returning every new log line, sorted by the underlying
    /// event's own timestamp.
    pub fn diff(&mut self, state: &Value, plan: &PlanTree) -> Vec<LogLine> {
        let flat = flatten_state(state);
        let mut lines = Vec::new();

        for (path, node) in &flat {
            let is_container = is_container(&node.meta);
            let event_sourced = self.event_sourced.contains(path);
            match self.last.get(path) {
                None => {
                    if is_container {
                        // A container gets no ▶/✓/✗ of its own, but a
                        // named `if`'s `meta.branch` is already set
                        // before its body even starts (DESIGN.md
                        // §1.2), and a loop's `meta.progress` can
                        // already be non-trivial the first time osp
                        // observes it at all (a fast first pass, or
                        // attaching mid-run) — both must still be
                        // checked on first sight, not only against a
                        // `prev` this branch never reaches (F9).
                        if let Some(branch) = &node.branch {
                            lines.push(LogLine {
                                ts: node.created_at.clone(),
                                text: format!("◆ {path} → {branch}"),
                            });
                        }
                        if let (Some(done), Some(total)) = (node.progress_done, node.progress_total)
                        {
                            let eta = node
                                .progress_eta_s
                                .map(|e| format!("{e:.1}s"))
                                .unwrap_or_else(|| "?".to_string());
                            lines.push(LogLine {
                                ts: node
                                    .completed_at
                                    .clone()
                                    .or_else(|| node.created_at.clone()),
                                text: format!("⟳ {path} pass {done}/{total}  ETA {eta}"),
                            });
                        }
                        continue;
                    }
                    if event_sourced {
                        continue;
                    }
                    // A path appearing for the first time.
                    if node.is_running() {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("▶ {path}  {}", summary_for(node)),
                        });
                    } else {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("▶ {path}  {}", summary_for(node)),
                        });
                        lines.push(end_line(path, node, plan));
                    }
                }
                Some(prev) => {
                    if !is_container && !event_sourced {
                        if prev.is_running() && !node.is_running() {
                            lines.push(end_line(path, node, plan));
                        } else if !prev.is_running() && !node.is_running() {
                            // A complete node whose `created_at` moved: the
                            // unnamed-loop-path reuse case (DESIGN.md §2.3).
                            if prev.created_at != node.created_at {
                                let pass =
                                    self.unnamed_pass_counts.entry(path.clone()).or_insert(0);
                                *pass += 1;
                                lines.push(LogLine {
                                    ts: node.created_at.clone(),
                                    text: format!("▶ {path} #{pass}  {}", summary_for(node)),
                                });
                                lines.push(end_line_numbered(path, node, plan, *pass));
                            }
                        }
                    }
                    // A retry's own `↻` line is *not* gated on
                    // `event_sourced` (N2): once `--events` is
                    // flowing, every leaf becomes event-sourced as
                    // soon as its `start` arrives (there's no earlier
                    // `prev` without a start to fall back to), and
                    // events carry no retry signal at all (design Q4)
                    // — a tool's `created_at` moving while it's still
                    // running is the *only* source for this line
                    // either way, so gating it the same as the
                    // events-redundant start/end lines above left it
                    // unreachable for the entire life of a run with
                    // `--events` on.
                    if !is_container
                        && prev.is_running()
                        && node.is_running()
                        && prev.created_at != node.created_at
                    {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("↻ {path} retry"),
                        });
                    }

                    if prev.branch.is_none() && node.branch.is_some() {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("◆ {path} → {}", node.branch.as_deref().unwrap_or("")),
                        });
                    }

                    if prev.progress_done != node.progress_done {
                        if let (Some(done), Some(total)) = (node.progress_done, node.progress_total)
                        {
                            let eta = node
                                .progress_eta_s
                                .map(|e| format!("{e:.1}s"))
                                .unwrap_or_else(|| "?".to_string());
                            lines.push(LogLine {
                                ts: node
                                    .completed_at
                                    .clone()
                                    .or_else(|| node.created_at.clone()),
                                text: format!("⟳ {path} pass {done}/{total}  ETA {eta}"),
                            });
                        }
                    }

                    // P2-6: a container's own failure (an each loop
                    // over a path that doesn't resolve, a CEL error in
                    // an if) is otherwise never logged at all --
                    // containers get no check/cross mark of their own,
                    // and under on_error: continue no child ever even
                    // starts to print one in its place. Only a
                    // container whose failure is really just a
                    // child's failure bubbling up (the far more common
                    // case) is left alone, since that child's own
                    // cross-mark line already said so.
                    if is_container
                        && prev.is_running()
                        && !node.is_running()
                        && node.error.is_some()
                        && !any_descendant_failed(path, &flat)
                    {
                        lines.push(LogLine {
                            ts: node.completed_at.clone(),
                            text: format!(
                                "✗ {path}  {}",
                                truncate_chars(node.error.as_deref().unwrap_or(""), 120)
                            ),
                        });
                    }
                }
            }
        }

        if !self.run_line_emitted && run_ended(state) && self.open_event_paths.is_empty() {
            self.run_line_emitted = true;
            let ok = run_ok(state);
            let totals = run_totals(state);
            let totals_text = totals
                .as_ref()
                .and_then(|t| t.get("wall_time_s"))
                .and_then(Value::as_f64)
                .map(|w| format!("  {w:.1}s"))
                .unwrap_or_default();
            let status_text = if ok {
                "ok".to_string()
            } else {
                format!("failed: {}", run_error(state).unwrap_or_default())
            };
            let run_ts = state
                .pointer("/runtime/last_run/completed_at")
                .and_then(Value::as_str)
                .map(str::to_string);
            lines.push(LogLine {
                ts: run_ts,
                text: format!("■ run {status_text}{totals_text}"),
            });
        }

        self.last = flat;
        sort_log_lines(&mut lines);
        lines
    }
}

impl Default for Differ {
    fn default() -> Self {
        Self::new()
    }
}

fn end_line(path: &str, node: &NodeMeta, plan: &PlanTree) -> LogLine {
    end_line_impl(path, node, plan, None)
}

fn end_line_numbered(path: &str, node: &NodeMeta, plan: &PlanTree, pass: u32) -> LogLine {
    end_line_impl(path, node, plan, Some(pass))
}

fn end_line_impl(path: &str, node: &NodeMeta, plan: &PlanTree, pass: Option<u32>) -> LogLine {
    let suffix = pass.map(|p| format!(" #{p}")).unwrap_or_default();
    let text = if let Some(error) = &node.error {
        format!(
            "✗ {path}{suffix}  {}{}",
            truncate_chars(error, 120),
            on_error_suffix(path, plan)
        )
    } else {
        format!(
            "✓ {path}{suffix}  {}  {}",
            duration_text(&node.created_at, &node.completed_at),
            result_summary(node)
        )
    };
    LogLine {
        ts: node.completed_at.clone(),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_chars_cuts_at_the_first_newline_too() {
        // F13: a multi-line error or tool stdout must not flow into
        // the log as several lines, each breaking the mm:ss.s shape.
        assert_eq!(truncate_chars("line one\nline two", 120), "line one");
        assert_eq!(truncate_chars("short", 120), "short");
    }

    #[test]
    fn new_running_node_gets_a_start_line() {
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell",
                "params_rendered": {"command": "sleep", "args": ["1"]}}}
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        let step1_line = lines
            .iter()
            .find(|l| l.text.starts_with("▶ prime.step1"))
            .expect("start line");
        assert!(step1_line.text.contains("sleep"));
    }

    #[test]
    fn completing_node_gets_a_done_line() {
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "step1": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&running, &PlanTree::empty());

        let done = json!({"prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
            "step1": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell", "stdout": "ok\n"}}
        }});
        let lines = differ.diff(&done, &PlanTree::empty());
        assert!(lines.iter().any(|l| l.text.starts_with("✓ prime.step1")));
    }

    #[test]
    fn failed_node_with_on_error_continue_gets_the_suffix() {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region};

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
                        on_error: OnError::Continue,
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

        let mut differ = Differ::new();
        let state = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "flaky: boom"},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom"}}
        }});
        let lines = differ.diff(&state, &plan);
        assert!(lines.iter().any(|l| l.text.starts_with("▶ prime.flaky")));
        let end = lines
            .iter()
            .find(|l| l.text.starts_with("✗ prime.flaky"))
            .expect("end line");
        assert!(end.text.contains("(on_error: continue)"));
    }

    #[test]
    fn failed_node_with_on_error_skip_gets_its_own_suffix() {
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region};

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

        let mut differ = Differ::new();
        let state = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "flaky: boom"},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom"}}
        }});
        let lines = differ.diff(&state, &plan);
        let end = lines
            .iter()
            .find(|l| l.text.starts_with("✗ prime.flaky"))
            .expect("end line");
        assert!(end.text.contains("(on_error: skip)"), "{end:?}");
    }

    #[test]
    fn a_containers_own_failure_with_no_failed_child_gets_a_cross_mark() {
        // P2-6: an each loop over a path that never resolves fails
        // the container itself without ever starting a single pass --
        // no child ever gets its own ✗, so without this the failure
        // is entirely invisible.
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "each": {"value": null, "meta": {"completed_at": null, "mode": "each"}}
        }});
        differ.diff(&running, &PlanTree::empty());

        let failed = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "each: path.that.never.resolved"},
            "each": {"value": null, "meta": {"completed_at": "t1", "mode": "each", "error": "path.that.never.resolved"}}
        }});
        let lines = differ.diff(&failed, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text.starts_with("✗ prime.each")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_containers_failure_from_a_failed_child_gets_no_extra_cross_mark() {
        // The far more common case: the container's own error is just
        // its child's failure bubbling up. That child's own ✗ line
        // already said so -- the container doesn't need a second one.
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "guarded": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "g_fail": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
            }
        }});
        differ.diff(&running, &PlanTree::empty());

        let failed = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "guarded.g_fail: boom"},
            "guarded": {"value": null, "meta": {"completed_at": "t1", "flow": "chain", "error": "g_fail: boom"},
                "g_fail": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom"}}
            }
        }});
        let lines = differ.diff(&failed, &PlanTree::empty());
        assert!(
            lines
                .iter()
                .any(|l| l.text.starts_with("✗ prime.guarded.g_fail"))
        );
        assert!(
            !lines.iter().any(|l| l.text.starts_with("✗ prime.guarded ")
                || l.text.starts_with("✗ prime.guarded  ")),
            "the container itself should get no extra cross mark: {lines:?}"
        );
    }

    #[test]
    fn a_container_node_gets_no_start_or_end_line_of_its_own() {
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "step1": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let lines = differ.diff(&running, &PlanTree::empty());
        assert!(
            !lines
                .iter()
                .any(|l| l.text.contains("prime ") || l.text == "▶ prime")
        );
        assert!(lines.iter().any(|l| l.text.starts_with("▶ prime.step1")));

        let done = json!({"prime": {"value": true, "meta": {"completed_at": "t1", "error": null, "flow": "chain"},
            "step1": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let lines2 = differ.diff(&done, &PlanTree::empty());
        assert!(
            !lines2
                .iter()
                .any(|l| l.text.starts_with("✓ prime ") || l.text.starts_with("✓ prime  "))
        );
        assert!(lines2.iter().any(|l| l.text.starts_with("✓ prime.step1")));
    }

    #[test]
    fn a_run_that_ended_with_no_prime_node_never_prints_run_ok() {
        // K6: a document invalid enough that nothing ever ran still
        // sets runtime.last_run.completed_at in its final write, with
        // no `prime` key at all. The run summary must say "failed",
        // never "ok", right before the engine's own nonzero exit code.
        let mut differ = Differ::new();
        let state = json!({"runtime": {"last_run": {"completed_at": "t9"}}});
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text.starts_with("■ run failed")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.text.starts_with("■ run ok")),
            "{lines:?}"
        );
    }

    #[test]
    fn the_run_line_waits_for_an_open_event_sourced_path_to_close_first() {
        // K5: state and events are two independently polled files; a
        // tick can see the state's final write (the run container
        // itself is done) before the events tailer catches up on the
        // very last leaf's own `end`. The run summary must not print
        // while any event-sourced path is still open, or it prints
        // *before* that leaf's own ✓ line instead of after.
        let mut differ = Differ::new();
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.last_leaf".to_string(),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );

        let state = json!({
            "runtime": {"last_run": {"completed_at": "t9"}},
            "prime": {"value": true, "meta": {"completed_at": "t9", "error": null}}
        });
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            !lines.iter().any(|l| l.text.starts_with("■ run")),
            "the run line must wait for the open leaf to close: {lines:?}"
        );

        differ.diff_event(
            &Event::End {
                ts: "t9".to_string(),
                id: Some(1),
                path: "prime.last_leaf".to_string(),
                ok: true,
                ms: Some(5),
                error: None,
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );
        let lines2 = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines2.iter().any(|l| l.text.starts_with("■ run ok")),
            "{lines2:?}"
        );
    }

    #[test]
    fn a_run_end_event_releases_any_still_open_path_so_the_run_line_is_never_stuck() {
        // An effect interrupted mid-flight never gets its own `end` at
        // all; `run_end` must still unblock the summary line.
        let mut differ = Differ::new();
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.interrupted".to_string(),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );
        differ.diff_event(
            &Event::RunEnd {
                ts: "t1".to_string(),
                ok: false,
                error: Some("Interrupted (Ctrl-C/SIGINT)".to_string()),
                signal: Some("SIGINT".to_string()),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );

        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)"}}
        });
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text.starts_with("■ run failed")),
            "{lines:?}"
        );
    }

    #[test]
    fn run_summary_line_appears_once_the_run_ends() {
        let mut differ = Differ::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t9", "totals": {"wall_time_s": 1.5}}},
            "prime": {"value": true, "meta": {"completed_at": "t9", "error": null}}
        });
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(lines.iter().any(|l| l.text.starts_with("■ run ok")));

        // A second diff against the same final state emits nothing more.
        let lines2 = differ.diff(&state, &PlanTree::empty());
        assert!(!lines2.iter().any(|l| l.text.starts_with("■")));
    }

    #[test]
    fn a_named_if_already_branched_on_first_sight_gets_a_diamond_line() {
        // F9: `meta.branch` is already set the very first time osp
        // observes a named `if` at all (DESIGN.md §1.2), so there is no
        // earlier `prev` without a branch to compare against.
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "gate": {"value": null, "meta": {"completed_at": null, "mode": "cel", "branch": "then"},
                "chosen": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
            }
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text == "◆ prime.gate → then"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_loop_already_mid_progress_on_first_sight_gets_a_refresh_line() {
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "each_tree": {"value": null, "meta": {"completed_at": null, "mode": "each",
                "progress": {"done": 2, "total": 3, "eta_s": 0.5}}}
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines
                .iter()
                .any(|l| l.text.starts_with("⟳ prime.each_tree pass 2/3")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_running_nodes_moved_created_at_gets_a_retry_line() {
        let mut differ = Differ::new();
        let first = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&first, &PlanTree::empty());

        let retried = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t1", "completed_at": null, "provider": "shell"}}
        }});
        let lines = differ.diff(&retried, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text == "↻ prime.flaky retry"),
            "{lines:?}"
        );
    }

    #[test]
    fn diff_event_emits_no_line_for_a_named_container() {
        // DESIGN.md §1.4: `start`/`end` fire for the root and every
        // *named* container the same as for a leaf, but a container
        // gets no ▶/✓/✗ of its own — only visible once `--events` is
        // actually flowing (#423) and firing for more than leaves,
        // which the earlier hand-built-event-only tests never did.
        use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError, Op, Region, ToolOp};

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

        let mut differ = Differ::new();
        let start = differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime".to_string(),
            },
            &plan,
            &RunModel::new(),
        );
        assert!(start.is_empty(), "{start:?}");

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime".to_string(),
                ok: true,
                ms: Some(5),
                error: None,
            },
            &plan,
            &RunModel::new(),
        );
        assert!(end.is_empty(), "{end:?}");

        // A real leaf under that same plan still gets its lines.
        let leaf_start = differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(2),
                path: "prime.step1".to_string(),
            },
            &plan,
            &RunModel::new(),
        );
        assert_eq!(leaf_start.len(), 1);
    }

    #[test]
    fn diff_event_emits_start_and_end_lines() {
        let mut differ = Differ::new();
        let start = differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.fan.a".to_string(),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );
        assert_eq!(start.len(), 1);
        assert!(start[0].text.starts_with("▶ prime.fan.a"));
        assert_eq!(start[0].ts.as_deref(), Some("t0"));

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.fan.a".to_string(),
                ok: true,
                ms: Some(1200),
                error: None,
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );
        assert_eq!(end.len(), 1);
        assert!(end[0].text.starts_with("✓ prime.fan.a  1.2s"));

        let failed = differ.diff_event(
            &Event::End {
                ts: "t2".to_string(),
                id: Some(2),
                path: "prime.fan.b".to_string(),
                ok: false,
                ms: Some(5),
                error: Some("boom".to_string()),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );
        assert!(failed[0].text.starts_with("✗ prime.fan.b  boom"));
    }

    #[test]
    fn an_event_sourced_nodes_moved_created_at_still_gets_a_retry_line() {
        // N2: every leaf becomes event-sourced the moment its `start`
        // event arrives, and events carry no retry signal at all
        // (design Q4) — so gating `↻` on "not event-sourced", the same
        // as the start/end lines events already cover, made it
        // unreachable for the rest of the run once `--events` is on.
        let mut differ = Differ::new();
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.flaky".to_string(),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );

        let first = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&first, &PlanTree::empty());

        let retried = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t1", "completed_at": null, "provider": "shell"}}
        }});
        let lines = differ.diff(&retried, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text == "↻ prime.flaky retry"),
            "{lines:?}"
        );
    }

    #[test]
    fn diff_suppresses_its_own_lines_for_an_event_sourced_path() {
        // Once `diff_event` has reported a path's start/end, `diff`
        // itself must not print the same transition again from the
        // next state snapshot that happens to show it too (F1).
        let mut differ = Differ::new();
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.fan.a".to_string(),
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );

        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null},
                "a": {"value": "", "meta": {"created_at": "t0", "completed_at": "t1", "error": null}}
            }
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            !lines.iter().any(|l| l.text.contains("prime.fan.a")),
            "event-sourced path should not get a second line from diff: {lines:?}"
        );
    }
}
