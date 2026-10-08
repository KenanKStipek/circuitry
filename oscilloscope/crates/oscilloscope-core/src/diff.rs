//! Turns a live-state snapshot diff, or an `--events` line, into log
//! lines (DESIGN.md §2.4), sorted by `meta.created_at`/`completed_at`
//! rather than observation order.

use std::collections::BTreeMap;

use electricity_bytecode::OnError;
use serde_json::Value;

use crate::model::{NodeMeta, flatten_state, run_ended, run_error, run_ok, run_totals};
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

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
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

fn on_error_suffix(path: &str, plan: &PlanTree) -> &'static str {
    match plan
        .match_path(path)
        .and_then(|m| m.entries.first().map(|e| e.on_error))
    {
        Some(OnError::Continue) => "  (on_error: continue)",
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
}

impl Differ {
    pub fn new() -> Self {
        Differ {
            last: BTreeMap::new(),
            run_line_emitted: false,
            unnamed_pass_counts: BTreeMap::new(),
        }
    }

    /// Diffs `state` against the previous snapshot this instance was
    /// given, returning every new log line, sorted by the underlying
    /// event's own timestamp.
    pub fn diff(&mut self, state: &Value, plan: &PlanTree) -> Vec<LogLine> {
        let flat = flatten_state(state);
        let mut lines = Vec::new();

        for (path, node) in &flat {
            match self.last.get(path) {
                None => {
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
                    if prev.is_running() && !node.is_running() {
                        lines.push(end_line(path, node, plan));
                    } else if !prev.is_running() && !node.is_running() {
                        // A complete node whose `created_at` moved: the
                        // unnamed-loop-path reuse case (DESIGN.md §2.3).
                        if prev.created_at != node.created_at {
                            let pass = self.unnamed_pass_counts.entry(path.clone()).or_insert(0);
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
            lines.push(LogLine {
                ts: None,
                text: format!("■ run {status_text}{totals_text}"),
            });
        }

        self.last = flat;
        lines.sort_by(|a, b| a.ts.cmp(&b.ts));
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
}
