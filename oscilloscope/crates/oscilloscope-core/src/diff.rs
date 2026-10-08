//! Turns a live-state snapshot diff, or an `--events` line, into log
//! lines (DESIGN.md §2.4), sorted by `meta.created_at`/`completed_at`
//! rather than observation order.

use std::collections::{BTreeMap, BTreeSet};

use electricity_bytecode::OnError;
use serde_json::Value;

use crate::model::{NodeMeta, flatten_state, run_ended, run_error, run_ok, run_totals};
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
}

impl Differ {
    pub fn new() -> Self {
        Differ {
            last: BTreeMap::new(),
            run_line_emitted: false,
            unnamed_pass_counts: BTreeMap::new(),
            event_sourced: BTreeSet::new(),
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
    pub fn diff_event(&mut self, event: &Event, plan: &PlanTree) -> Vec<LogLine> {
        match event {
            Event::Start { ts, path, .. } => {
                self.event_sourced.insert(path.clone());
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
                        } else if prev.is_running()
                            && node.is_running()
                            && prev.created_at != node.created_at
                        {
                            lines.push(LogLine {
                                ts: node.created_at.clone(),
                                text: format!("↻ {path} retry"),
                            });
                        }
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
                }
            }
        }

        if !self.run_line_emitted && run_ended(state) {
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
    fn diff_event_emits_start_and_end_lines() {
        let mut differ = Differ::new();
        let start = differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.fan.a".to_string(),
            },
            &PlanTree::empty(),
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
        );
        assert!(failed[0].text.starts_with("✗ prime.fan.b  boom"));
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
