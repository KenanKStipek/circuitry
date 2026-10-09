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

/// Parses an RFC-3339 timestamp from either source format this crate
/// ever sees: `--events`' own millisecond-plus-`Z` timestamps and
/// state's microsecond-plus-offset ones. The two are not
/// lexicographically comparable as strings at all -- `Z` (0x5A) sorts
/// after every digit, so an event's own millisecond-truncated
/// timestamp can sort *after* a state timestamp in the very same
/// millisecond (M1) -- so `sort_log_lines` must compare real instants.
fn parse_instant(ts: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(ts).ok()
}

/// Sorts lines from two different observations (a state `diff` and an
/// `--events` line, each already timestamped from its own source) into
/// one chronological stream (DESIGN.md §2: "Log lines are sorted by
/// those times", never by which poll noticed them first). `None` (the
/// final run-summary line, when no timestamp was ever available at
/// all) sorts last. Falls back to a plain string compare only if a
/// timestamp doesn't parse as RFC-3339 at all, which no real source
/// here ever produces.
pub fn sort_log_lines(lines: &mut [LogLine]) {
    lines.sort_by(|a, b| match (&a.ts, &b.ts) {
        (Some(x), Some(y)) => match (parse_instant(x), parse_instant(y)) {
            (Some(ix), Some(iy)) => ix.cmp(&iy),
            _ => x.cmp(y),
        },
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

/// Caps a possibly multi-line failure reason to `max` characters,
/// keeping its newlines (M2) -- unlike `truncate_chars`, which
/// collapses an effect's own error to one line, the run summary's own
/// reason (cof's pre-execution validation JSON, or a run_end event's)
/// can be several lines long and still worth keeping, just indented
/// under the first.
fn truncate_reason(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

/// The run summary's own failure reason (M2, DESIGN.md §2.4's `■ run
/// ...` row): the first line goes right on the `■` line itself, and any
/// further lines print indented under it, rather than either losing
/// them or breaking the log's one-timestamped-line-per-entry shape.
pub fn format_reason_for_summary(reason: &str) -> String {
    let capped = truncate_reason(reason, 500);
    let mut lines = capped.lines();
    let first = lines.next().unwrap_or("");
    let mut out = first.to_string();
    for line in lines {
        out.push_str("\n  ");
        out.push_str(line);
    }
    out
}

/// The plan's own label for `path` (DESIGN.md §6.2/§6.3's "show the
/// effect kind from the plan ... instead of `effect`", #433) —
/// `summary_for`'s own fallback when a leaf's `meta` carries none of
/// the shape-based clues (`provider`, `adapter`) it otherwise reads,
/// and `diff_event`'s fallback for a `▶` line whose path has no state
/// at all yet. `"effect"` remains the fallback with no plan, or for a
/// path the plan doesn't recognise (an inline `use` child, a
/// reflector pass) — both discovered from observations alone.
fn plan_kind_label(path: &str, plan: &PlanTree) -> &'static str {
    match plan.match_path(path).and_then(|m| m.entries.first()) {
        Some(entry) => match entry.kind {
            crate::plan::PlanEntryKind::Leaf(kind) => kind.label(),
            _ => "effect",
        },
        None => "effect",
    }
}

fn summary_for(node: &NodeMeta, path: &str, plan: &PlanTree) -> String {
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
    plan_kind_label(path, plan).to_string()
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
            return Some(!matches!(entry.kind, crate::plan::PlanEntryKind::Leaf(_)));
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
fn any_descendant_failed(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    event_failed: &BTreeSet<String>,
) -> bool {
    let prefix = format!("{path}.");
    flat.iter()
        .any(|(other, node)| other.starts_with(&prefix) && node.error.is_some())
        || event_failed.iter().any(|other| other.starts_with(&prefix))
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
    /// Paths whose own `end` event reported `ok: false` (M1b, a P2-6
    /// follow-up): `any_descendant_failed` must see a sibling's
    /// failure from earlier in the *same* tick, not just what `diff`'s
    /// state-sourced `self.last` already knew about as of the previous
    /// tick. Events in one tick are handled one at a time, in file
    /// order, strictly before `diff` ever updates `self.last` from
    /// that tick's own state poll -- so a child's own ✗, two calls
    /// earlier in this same batch, was otherwise invisible to its
    /// container's `end` landing right after it, producing a second,
    /// duplicate ✗ for the container.
    event_failed: BTreeSet<String>,
}

impl Differ {
    pub fn new() -> Self {
        Differ {
            last: BTreeMap::new(),
            run_line_emitted: false,
            unnamed_pass_counts: BTreeMap::new(),
            event_sourced: BTreeSet::new(),
            event_failed: BTreeSet::new(),
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
                    .map(|node| summary_for(node, path, plan))
                    .unwrap_or_else(|| plan_kind_label(path, plan).to_string());
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
                if !*ok {
                    // M1b: recorded *before* the container branch
                    // below checks it, so a child's own failure a
                    // couple of events earlier in this same batch is
                    // already visible here, even though `diff`'s own
                    // state-sourced `self.last` won't catch up until
                    // this tick's later `diff` call.
                    self.event_failed.insert(path.clone());
                }
                if is_container_path(path, plan, &self.last, model) {
                    // P2-6: the container's own failure, when it has
                    // one, still needs a line if nothing under it
                    // already printed one -- the events path has the
                    // exact same gap state's `diff` does.
                    if !*ok && !any_descendant_failed(path, &self.last, &self.event_failed) {
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
            Event::RunEnd { .. } => Vec::new(),
            _ => Vec::new(),
        }
    }

    /// Whether `finish` has already produced the `■ run ...` summary
    /// line (F5): lets a caller fall back to a synthetic summary —
    /// "aborted", or a reason it reads some other way — only when
    /// `finish` never had an ended run to report one from at all.
    pub fn run_line_emitted(&self) -> bool {
        self.run_line_emitted
    }

    /// The run's own summary line (DESIGN.md §2.4's `■ run ...` row),
    /// computed exactly once, from whichever final state the caller
    /// hands it (M1): `diff` itself never emits this line mid-run any
    /// more. A per-tick "every event-sourced path has closed" gate used
    /// to approximate "the run is really over" from inside the polling
    /// loop, but state and events are two independently polled files —
    /// the state's final write (the run container itself marked done)
    /// can land a tick ahead of the events tailer catching up on the
    /// very last leaf's own `end`, printing this line before that
    /// leaf's own `✓`/`✗`. Calling this only once, after the caller has
    /// already fully drained both files at the engine's own exit,
    /// removes the race by construction instead of narrowing its
    /// window.
    ///
    /// `fallback_reason` (M2) fills in `prime.meta.error`'s absence — a
    /// run that ended (`runtime.last_run.completed_at` set) with no
    /// `prime` node at all (a document invalid enough that nothing
    /// ran) has no error of its own to report. The caller supplies one
    /// in priority order: a `run_end` event's own `error`, then cof's
    /// pre-execution stdout JSON, then the engine's last stderr line.
    pub fn finish(&mut self, state: &Value, fallback_reason: Option<&str>) -> Option<LogLine> {
        if self.run_line_emitted || !run_ended(state) {
            return None;
        }
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
            match run_error(state).filter(|e| !e.is_empty()) {
                Some(reason) => format!("failed: {}", format_reason_for_summary(&reason)),
                None => match fallback_reason.filter(|r| !r.is_empty()) {
                    Some(reason) => format!("failed: {}", format_reason_for_summary(reason)),
                    None => "failed".to_string(),
                },
            }
        };
        let run_ts = state
            .pointer("/runtime/last_run/completed_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(LogLine {
            ts: run_ts,
            text: format!("■ run {status_text}{totals_text}"),
        })
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
                            text: format!("▶ {path}  {}", summary_for(node, path, plan)),
                        });
                    } else {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("▶ {path}  {}", summary_for(node, path, plan)),
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
                                    text: format!(
                                        "▶ {path} #{pass}  {}",
                                        summary_for(node, path, plan)
                                    ),
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
                        && !any_descendant_failed(path, &flat, &self.event_failed)
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

    /// #433: a `yield` leaf's own `meta` carries neither `provider`
    /// nor `adapter`, so with no plan at all `summary_for` has nothing
    /// to go on but the generic `"effect"` fallback (DESIGN.md §6.2's
    /// example never needs it: every leaf there is a tool or a
    /// prompt) — but with a real plan, osp can and should say
    /// `"yield"` instead.
    #[test]
    fn a_yield_leaf_with_no_distinguishing_meta_takes_its_kind_from_the_plan() {
        use electricity_bytecode::{
            EffectPath, Escape, LeafKind, NodeKind, OnError, Op, Region, TemplateText, YieldOp,
        };

        let root_path = EffectPath::root();
        let child_path = root_path.clone().push_name("announce");
        let program = electricity_bytecode::Program {
            root: Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: child_path,
                        name: Some("announce".to_string()),
                        kind: NodeKind::Leaf(Box::new(LeafKind::Yield(YieldOp {
                            template: TemplateText::new("hi", true, Escape::None),
                            inputs: None,
                            description: None,
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
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "announce": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
        }});
        let lines = differ.diff(&state, &plan);
        let line = lines
            .iter()
            .find(|l| l.text.starts_with("▶ prime.announce"))
            .expect("start line");
        assert_eq!(line.text, "▶ prime.announce  yield");
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
        let line = differ.finish(&state, None).expect("a summary line");
        assert!(line.text.starts_with("■ run failed"), "{line:?}");
        assert!(!line.text.starts_with("■ run ok"), "{line:?}");
    }

    #[test]
    fn finish_returns_none_until_the_caller_has_an_ended_state() {
        // M1: `finish` itself does no waiting at all -- unlike the old
        // per-tick heuristic this replaces, it never guesses whether a
        // run is "really" over from an in-flight state. It is the
        // caller's job (main.rs's `do_run`/`do_watch`) to call it only
        // after draining both files to their true end, once.
        let mut differ = Differ::new();
        let mid_run = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        assert!(differ.finish(&mid_run, None).is_none());

        let ended = json!({
            "runtime": {"last_run": {"completed_at": "t9"}},
            "prime": {"value": true, "meta": {"completed_at": "t9", "error": null}}
        });
        let line = differ.finish(&ended, None).expect("a summary line");
        assert!(line.text.starts_with("■ run ok"), "{line:?}");
    }

    #[test]
    fn finish_reports_an_interrupted_run_as_failed_even_with_an_open_event_sourced_path() {
        // M1: there is no `open_event_paths` gate left to release --
        // an effect interrupted mid-flight never gets its own `end`
        // event at all, but that no longer matters, since the caller
        // decides when to call `finish`, from a state it already
        // knows is final.
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
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)"}}
        });
        let line = differ.finish(&state, None).expect("a summary line");
        assert!(line.text.starts_with("■ run failed"), "{line:?}");
    }

    #[test]
    fn diff_event_container_gets_no_extra_cross_mark_when_its_child_failed_in_the_same_batch() {
        // M1b (a P2-6 follow-up): the container's own failing `end`
        // event, landing in the same batch as its child's own, must
        // not get a second ✗ -- `self.last` (the state diff's own
        // record) hasn't caught up yet in the same tick, so
        // `any_descendant_failed` needs `event_failed` to see the
        // child's failure instead.
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(0),
                path: "prime".to_string(),
            },
            &plan,
            &model,
        );
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.boom".to_string(),
            },
            &plan,
            &model,
        );
        let child_lines = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.boom".to_string(),
                ok: false,
                ms: Some(1),
                error: Some("boom".to_string()),
            },
            &plan,
            &model,
        );
        let container_lines = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(0),
                path: "prime".to_string(),
                ok: false,
                ms: Some(2),
                error: Some("prime.boom: boom".to_string()),
            },
            &plan,
            &model,
        );
        assert!(
            child_lines
                .iter()
                .any(|l| l.text.starts_with("✗ prime.boom")),
            "{child_lines:?}"
        );
        assert!(
            container_lines.is_empty(),
            "the container must get no extra cross mark: {container_lines:?}"
        );
    }

    #[test]
    fn finish_keeps_a_multi_line_reasons_first_line_on_the_summary_and_indents_the_rest() {
        // M2: cof's own validation error can be several lines long;
        // the first goes right on the ■ line, the rest print indented
        // under it rather than either being lost or breaking the log's
        // one-timestamped-line-per-entry shape.
        let mut differ = Differ::new();
        let state = json!({"runtime": {"last_run": {"completed_at": "t9"}}});
        let line = differ
            .finish(&state, Some("line one\nline two"))
            .expect("a summary line");
        assert_eq!(line.text, "■ run failed: line one\n  line two");
    }

    #[test]
    fn run_summary_line_appears_once_the_run_ends() {
        let mut differ = Differ::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t9", "totals": {"wall_time_s": 1.5}}},
            "prime": {"value": true, "meta": {"completed_at": "t9", "error": null}}
        });
        let line = differ.finish(&state, None);
        assert!(line.is_some_and(|l| l.text.starts_with("■ run ok")));

        // A second call against the same final state emits nothing more.
        assert!(differ.finish(&state, None).is_none());
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
