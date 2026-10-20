//! Turns a live-state snapshot diff, or an `--events` line, into log
//! lines (DESIGN.md §2.4), sorted by `meta.created_at`/`completed_at`
//! rather than observation order.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use electricity_bytecode::OnError;
use serde_json::Value;

use crate::model::{
    EndedOutcome, NodeMeta, RunModel, ended_outcome, flatten_state, run_ended, run_error,
    run_totals,
};
use crate::observe::{Event, ReportLevel};
use crate::plan::PlanTree;

/// One line of the `--log` stream (DESIGN.md §2.4, §6.2). `ts` is the
/// event's own timestamp (`meta.created_at`/`completed_at`, never when
/// osi noticed the change) — `None` only for the final run-summary
/// line when no timestamp is available at all. `--log` itself prints
/// only `ts`/`text`, byte for byte as before (issue #635) — `kind`
/// and `path` exist purely for a front end (the Flow screen's story)
/// to classify a line without re-parsing `text`'s own glyph prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub ts: Option<String>,
    pub text: String,
    pub kind: LogKind,
    /// The line's own dotted path, when it has one — `None` for a
    /// run-start/run-end line, which names no step.
    pub path: Option<String>,
}

/// A [`LogLine`]'s own category (issue #635's story: "run start, each
/// decision, each report, each failure, run end", oldest first) —
/// every other kind (`Start`/`End`/`Progress`/`Retry`) still gets a
/// `LogLine` for `--log`, just never one the story keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    RunStart,
    RunEnd,
    Start,
    End,
    Failure,
    Decision,
    Progress,
    Retry,
    Report,
}

/// Parses an RFC-3339 timestamp from either source format this crate
/// ever sees: `--events`' own millisecond-plus-`Z` timestamps and
/// state's microsecond-plus-offset ones. The two are not
/// lexicographically comparable as strings at all -- `Z` (0x5A) sorts
/// after every digit, so an event's own millisecond-truncated
/// timestamp can sort *after* a state timestamp in the very same
/// millisecond (M1) -- so `sort_log_lines` must compare real instants.
pub fn parse_instant(ts: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
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
/// one-line-per-entry shape every other entry keeps (F13).
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

/// A report's own `data`'s cap in `--log` (#610): the TUI applies
/// a further, tighter clip of its own at draw time, to the pane's
/// real width -- this is the one true cap for plain `--log`, where
/// there is no pane to fit at all.
const REPORT_DATA_LOG_CHARS: usize = 200;

/// A report's own `message`'s cap in `--log` (#610), the same rule
/// `truncate_chars` already applies to an effect's own error text
/// (DESIGN.md §2.4's "one line, with a character cap").
const REPORT_MESSAGE_LOG_CHARS: usize = 120;

/// A report's own log line (#610): `▸ <message>`, `▸ warning:
/// <message>` for a warning -- the leading `▸` is never used for any
/// other kind of line this crate ever builds, so a front end can
/// still tell a report line apart from every other one by its text
/// alone, with no extra field on [`LogLine`] needed. `data`, when the
/// report has any and it wasn't itself truncated by the engine, is a
/// compact one-line JSON preview appended after two spaces, capped to
/// [`REPORT_DATA_LOG_CHARS`] here; `data_truncated` (set by the
/// engine once `data` itself exceeds 16 KiB) shows `(data truncated)`
/// instead, since there is nothing left of `data` to preview at all.
fn report_line(
    level: ReportLevel,
    message: &str,
    data: Option<&Value>,
    data_truncated: bool,
) -> String {
    let prefix = match level {
        ReportLevel::Info => String::new(),
        ReportLevel::Warning => "warning: ".to_string(),
    };
    // A Mustache-rendered message can contain a newline; `truncate_chars`
    // keeps only its first line and caps it, the same rule every other
    // `--log` entry's own value already follows (DESIGN.md §2.4: "one
    // line, with a character cap") -- a report's message had neither
    // before this (#610).
    let message = truncate_chars(message, REPORT_MESSAGE_LOG_CHARS);
    let mut text = format!("▸ {prefix}{message}");
    if data_truncated {
        text.push_str("  (data truncated)");
    } else if let Some(data) = data {
        let json = serde_json::to_string(data).unwrap_or_default();
        text.push_str("  ");
        text.push_str(&truncate_reason(&json, REPORT_DATA_LOG_CHARS));
    }
    text
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
        return format!(
            "↑{} ↓{}",
            crate::format::format_tokens(tokens_sent),
            crate::format::format_tokens(received)
        );
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
    event_failed: &BTreeSet<(String, i64)>,
    path_failed_without_id: &BTreeSet<String>,
) -> bool {
    let prefix = format!("{path}.");
    flat.iter()
        .any(|(other, node)| other.starts_with(&prefix) && node.error.is_some())
        || event_failed
            .iter()
            .any(|(other, _)| other.starts_with(&prefix))
        || path_failed_without_id
            .iter()
            .any(|other| other.starts_with(&prefix))
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
            Some(secs) => crate::format::format_duration(secs),
            None => "?".to_string(),
        },
        _ => "?".to_string(),
    }
}

/// How long [`Differ::settle_pending_ends`] holds an `End` line
/// waiting for its node's state before giving up and falling back to
/// the old `ok` text (issue #545's "Defer" fix, chosen over "re-read
/// once" because neither engine writes a leaf's state before it emits
/// that leaf's own `end`: `cof`'s chain loop calls `store.on_write`
/// from a `finally:` strictly *after* `Store.fire_effect_complete`
/// returns, and electricity's every exec module calls
/// `observer.write()` strictly after `observer.effect_complete(...)`,
/// which only *marks* the live-state mirror pending). Both engines
/// coalesce the actual write to disk to once per 500ms
/// (`cli/live_state.py`'s `LIVE_STATE_INTERVAL_SECONDS` and
/// `live_state.rs`'s `LIVE_STATE_INTERVAL`), and osi polls that file
/// every `observe::POLL_INTERVAL` (100ms) — a line can therefore need
/// to outlive two full coalescing windows, not just one more poll, so
/// the bound is wall-clock time, not a poll count: two coalescing
/// windows plus one more poll to be sure of catching the write once
/// it lands.
const END_SUMMARY_MAX_WAIT: Duration = Duration::from_millis(2 * 500 + 100);

/// An `End{ok: true}` line [`Differ::diff_event`] couldn't format yet
/// because neither `last` nor `preview` has the path's completed
/// state (#545) — held here, re-checked by
/// [`Differ::settle_pending_ends`] every later tick, until either the
/// state catches up or `END_SUMMARY_MAX_WAIT` runs out.
#[derive(Clone)]
struct PendingEnd {
    ts: String,
    path: String,
    /// Pre-rendered (`ms.map(...)`), same text the immediate-resolve
    /// path already builds — nothing left to compute once the state
    /// shows up, only the result summary.
    duration: String,
    held_at: Instant,
}

/// Tracks each run's own progress across consecutive snapshots, so a
/// transition (a node appearing, completing, or moving) can be told
/// from the previous observation — DESIGN.md §2.4's diff table.
///
/// `Clone` exists for exactly one caller (issue #635): the TUI wants
/// the run-end story line as soon as the engine exits, without
/// retiring the real `Differ`'s own `run_line_emitted` latch before
/// `--log`'s authoritative, one-time `finish` call after the TUI
/// closes -- it calls `finish` on a clone instead, and throws the
/// clone away.
#[derive(Clone)]
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
    /// Every `(path, id)` instance whose own `end` event reported
    /// `ok: false` (M1b, a P2-6 follow-up; review finding 5 on #458's
    /// comment 4): `any_descendant_failed` must see a sibling's
    /// failure from earlier in the *same* tick, not just what `diff`'s
    /// state-sourced `self.last` already knew about as of the previous
    /// tick. Events in one tick are handled one at a time, in file
    /// order, strictly before `diff` ever updates `self.last` from
    /// that tick's own state poll -- so a child's own ✗, two calls
    /// earlier in this same batch, was otherwise invisible to its
    /// container's `end` landing right after it, producing a second,
    /// duplicate ✗ for the container. Keyed per *instance*, not just
    /// by path: a later `start` reusing the same path -- a retry, or
    /// the next pass of an unnamed loop (DESIGN.md §2.3) -- never
    /// erases a *different* id's own already-reported failure. Never
    /// cleared, which trades away catching the opposite case (a
    /// stale, long-done descendant failure hiding a later, genuinely
    /// unrelated container failure) for fixing the more visibly wrong
    /// one: a reused path's pass that already printed its own ✗ no
    /// longer causes its own container to print a redundant second
    /// one once that pass's own slot is reused.
    event_failed: BTreeSet<(String, i64)>,
    /// The same idea as `event_failed`, for an `end` with no `id` of
    /// its own at all (an engine build old enough to omit one): no
    /// instance key to track separately, so this keeps the previous
    /// per-path behaviour instead, cleared on that exact path's own
    /// next `start`.
    path_failed_without_id: BTreeSet<String>,
    /// The freshest state `note_preview` has been handed, kept
    /// separate from `last` (which only ever advances inside `diff`
    /// itself): container detection for an event whose own `start`
    /// outruns the next state poll (`osi watch` with no `--plan`,
    /// issue #433) needs the very latest snapshot available, even one
    /// `diff` hasn't been handed yet this tick.
    preview: BTreeMap<String, NodeMeta>,
    /// `End{ok: true}` lines still waiting on their node's own state
    /// (#545) — see [`PendingEnd`].
    pending_ends: Vec<PendingEnd>,
    /// Every path a `report` event has ever named (#610): `diff`'s own
    /// state walk has no engine/model to ask `PlanTree::is_report_path`
    /// couldn't already answer, so this stands in for it exactly where
    /// the plan has no real entry at that path -- a named report's
    /// state node must never grow its own `▶`/`✓` lines here, even
    /// under `osi watch` or a `use: inline:`/`ref:` child with no
    /// compiled plan to resolve it from at all.
    report_paths: BTreeSet<String>,
    /// Paths `diff` has already held for one poll (#616's comment):
    /// a report-shaped node with no plan entry and no report event
    /// yet, seen for the first time, is skipped rather than printed
    /// as a step -- `diff` keeps it out of `last` for that one poll so
    /// the next one re-enters the same "first sighting" choice. A
    /// path already in this set when seen that way again gets printed
    /// as an ordinary first sighting instead of held a second time,
    /// whether or not the event has arrived by then (it hasn't, or
    /// `report_paths` would already have made it `report_only` above).
    held_once: BTreeSet<String>,
}

impl Differ {
    pub fn new() -> Self {
        Differ {
            last: BTreeMap::new(),
            run_line_emitted: false,
            unnamed_pass_counts: BTreeMap::new(),
            event_sourced: BTreeSet::new(),
            event_failed: BTreeSet::new(),
            path_failed_without_id: BTreeSet::new(),
            preview: BTreeMap::new(),
            pending_ends: Vec::new(),
            report_paths: BTreeSet::new(),
            held_once: BTreeSet::new(),
        }
    }

    /// Records the latest polled state for [`Differ::diff_event`]'s
    /// own container detection to fall back on, ahead of `diff` itself
    /// ever being handed this same snapshot (issue #433: `osi watch`
    /// with no `--plan` could otherwise misjudge a container whose
    /// very first state write lands in the same tick as its own
    /// `start` event, since events are processed before that tick's
    /// `diff` call updates `last`). A no-op once a real plan exists,
    /// since `is_container_path` already answers from it before ever
    /// consulting state.
    pub fn note_preview(&mut self, state: &Value) {
        self.preview = flatten_state(state);
    }

    /// The freshest *completed* node known for `path`: `preview`
    /// (this tick's own poll, not yet diffed into `last`) first, then
    /// `last` (what `diff` has already processed) — `preview` is
    /// always at least as new as `last`, since `note_preview` runs
    /// ahead of `diff` every tick (`main.rs`'s `observe_tick`), so a
    /// tick whose `preview` already shows a node complete must not
    /// lose to a `last` still holding that same path's own previous,
    /// still-running snapshot. A node without `completed_at` set is
    /// treated the same as no entry at all: a chain leaf's state can
    /// land before its own `end` event does, still running
    /// (`completed_at: null`, no tokens yet) — resolving from that
    /// would print `ok` immediately, the exact race this exists to
    /// avoid, so such a node is skipped in favour of the pending hold
    /// below, the same as if neither map had the path yet.
    fn known_node(&self, path: &str) -> Option<&NodeMeta> {
        self.preview
            .get(path)
            .filter(|n| n.completed_at.is_some())
            .or_else(|| self.last.get(path).filter(|n| n.completed_at.is_some()))
    }

    /// Whether an `End` line is still waiting on its own completed
    /// state (#545).
    pub fn has_pending_ends(&self) -> bool {
        !self.pending_ends.is_empty()
    }

    /// Re-checks every held `End` line (#545, [`PendingEnd`]) against
    /// the state now known, resolving it with the real result summary
    /// as soon as that appears; a line still held longer than
    /// [`END_SUMMARY_MAX_WAIT`] falls back to the old `ok` text rather
    /// than holding forever. Call once per tick, whether or not that
    /// tick's own `diff`/`diff_event` calls found anything new — the
    /// aging only happens here. `now` is passed in, not read with
    /// `Instant::now()`, so a test can construct instants directly
    /// instead of actually waiting out the bound.
    pub fn settle_pending_ends(&mut self, now: Instant) -> Vec<LogLine> {
        let mut resolved = Vec::new();
        let pending = std::mem::take(&mut self.pending_ends);
        for pending_end in pending {
            if let Some(node) = self.known_node(&pending_end.path) {
                resolved.push(LogLine {
                    ts: Some(pending_end.ts),
                    text: format!(
                        "✓ {}  {}  {}",
                        pending_end.path,
                        pending_end.duration,
                        result_summary(node)
                    ),
                    kind: LogKind::End,
                    path: Some(pending_end.path.clone()),
                });
                continue;
            }
            if now.saturating_duration_since(pending_end.held_at) >= END_SUMMARY_MAX_WAIT {
                resolved.push(LogLine {
                    ts: Some(pending_end.ts),
                    text: format!("✓ {}  {}  ok", pending_end.path, pending_end.duration),
                    kind: LogKind::End,
                    path: Some(pending_end.path.clone()),
                });
                continue;
            }
            self.pending_ends.push(pending_end);
        }
        resolved
    }

    /// Resolves every still-held `End` line (#545) once, with no
    /// further ageing: called only after the run's authoritative
    /// final state has already been diffed (`main.rs`'s
    /// `drain_until_ended`, right after its own final `differ.diff`
    /// call), so a line's node is either known by now, from that
    /// final state, or it never will be — there is no "next tick" to
    /// wait for once the engine has already exited, so ageing a line
    /// out here based on how little wall-clock time the drain loop
    /// itself happened to take would abandon a line to `ok` even
    /// though its real tokens are sitting right there in the state
    /// just diffed.
    pub fn flush_pending_ends(&mut self) -> Vec<LogLine> {
        let mut resolved = Vec::new();
        for pending_end in std::mem::take(&mut self.pending_ends) {
            let text = match self.known_node(&pending_end.path) {
                Some(node) => format!(
                    "✓ {}  {}  {}",
                    pending_end.path,
                    pending_end.duration,
                    result_summary(node)
                ),
                None => format!("✓ {}  {}  ok", pending_end.path, pending_end.duration),
            };
            resolved.push(LogLine {
                ts: Some(pending_end.ts),
                text,
                kind: LogKind::End,
                path: Some(pending_end.path.clone()),
            });
        }
        resolved
    }

    /// The freshest state known for container detection at `path`:
    /// `last` (what `diff` has already processed) when it has an
    /// entry, `preview` (this tick's own poll, not yet diffed)
    /// otherwise.
    fn container_lookup(&self, path: &str) -> &BTreeMap<String, NodeMeta> {
        if self.last.contains_key(path) {
            &self.last
        } else {
            &self.preview
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
                // `event_failed` itself (the id'd set) is never
                // cleared here any more (review finding 5 on #458's
                // comment 4) -- only the id-less fallback is, the same
                // as before: a fresh `start` at this exact path clears
                // its own entry and every descendant's (a loop's path
                // is also every one of its children's own prefix).
                let restarted_prefix = format!("{path}.");
                self.path_failed_without_id.remove(path);
                self.path_failed_without_id
                    .retain(|other| !other.starts_with(&restarted_prefix));
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
                if is_container_path(path, plan, self.container_lookup(path), model) {
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
                    kind: LogKind::Start,
                    path: Some(path.clone()),
                }]
            }
            Event::End {
                ts,
                id,
                path,
                ok,
                ms,
                error,
            } => {
                self.event_sourced.insert(path.clone());
                if !*ok {
                    // M1b: recorded *before* the container branch
                    // below checks it, so a child's own failure a
                    // couple of events earlier in this same batch is
                    // already visible here, even though `diff`'s own
                    // state-sourced `self.last` won't catch up until
                    // this tick's later `diff` call.
                    match id {
                        Some(id) => {
                            self.event_failed.insert((path.clone(), *id));
                        }
                        None => {
                            self.path_failed_without_id.insert(path.clone());
                        }
                    }
                }
                if is_container_path(path, plan, self.container_lookup(path), model) {
                    // P2-6: the container's own failure, when it has
                    // one, still needs a line if nothing under it
                    // already printed one -- the events path has the
                    // exact same gap state's `diff` does.
                    if !*ok
                        && !any_descendant_failed(
                            path,
                            &self.last,
                            &self.event_failed,
                            &self.path_failed_without_id,
                        )
                    {
                        let message = error.as_deref().unwrap_or("");
                        return vec![LogLine {
                            ts: Some(ts.clone()),
                            text: format!("✗ {path}  {}", truncate_chars(message, 120)),
                            kind: LogKind::Failure,
                            path: Some(path.clone()),
                        }];
                    }
                    return Vec::new();
                }
                if *ok {
                    let duration = ms
                        .map(|m| crate::format::format_duration(m as f64 / 1000.0))
                        .unwrap_or_else(|| "?".to_string());
                    if let Some(node) = self.known_node(path) {
                        return vec![LogLine {
                            ts: Some(ts.clone()),
                            text: format!("✓ {path}  {duration}  {}", result_summary(node)),
                            kind: LogKind::End,
                            path: Some(path.clone()),
                        }];
                    }
                    // Neither `last` nor `preview` has this path's
                    // completed state yet (#545: a fast prompt's `end`
                    // event can arrive before either engine's own
                    // live-state write for it does — see
                    // `END_SUMMARY_MAX_WAIT`'s own doc comment). Hold
                    // the line rather than guessing "ok":
                    // `settle_pending_ends` revisits it every later
                    // tick.
                    self.pending_ends.push(PendingEnd {
                        ts: ts.clone(),
                        path: path.clone(),
                        duration,
                        held_at: Instant::now(),
                    });
                    return Vec::new();
                }
                let message = error.as_deref().unwrap_or("");
                vec![LogLine {
                    ts: Some(ts.clone()),
                    text: format!(
                        "✗ {path}  {}{}",
                        truncate_chars(message, 120),
                        on_error_suffix(path, plan)
                    ),
                    kind: LogKind::Failure,
                    path: Some(path.clone()),
                }]
            }
            Event::Report {
                ts,
                path,
                level,
                message,
                data,
                data_truncated,
            } => {
                self.report_paths.insert(path.clone());
                vec![LogLine {
                    ts: Some(ts.clone()),
                    text: report_line(*level, message, data.as_ref(), *data_truncated),
                    kind: LogKind::Report,
                    path: Some(path.clone()),
                }]
            }
            Event::RunStart { ts, run_id, .. } => vec![LogLine {
                ts: Some(ts.clone()),
                text: format!("■ run started  {run_id}"),
                kind: LogKind::RunStart,
                path: None,
            }],
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
    ///
    /// The ok/cancelled/failed split is `ended_outcome` (DESIGN.md
    /// §2.1 rule 7's first bullet), the same one `run_status` uses for
    /// the TUI header — a run the user cancelled must read "cancelled"
    /// here too, not "failed".
    pub fn finish(&mut self, state: &Value, fallback_reason: Option<&str>) -> Option<LogLine> {
        if self.run_line_emitted || !run_ended(state) {
            return None;
        }
        self.run_line_emitted = true;
        let totals = run_totals(state);
        let totals_text = totals
            .as_ref()
            .and_then(|t| t.get("wall_time_s"))
            .and_then(Value::as_f64)
            .map(|w| format!("  {}", crate::format::format_duration(w)))
            .unwrap_or_default();
        let status_text = match ended_outcome(state) {
            EndedOutcome::Ok => "ok".to_string(),
            EndedOutcome::Cancelled => {
                // `ended_outcome` only returns `Cancelled` when
                // `run_error` is `Some` and starts with "Interrupted",
                // so this is always present — no fallback needed.
                let reason = run_error(state).unwrap_or_default();
                format!("cancelled: {}", format_reason_for_summary(&reason))
            }
            EndedOutcome::Failed => match run_error(state).filter(|e| !e.is_empty()) {
                Some(reason) => format!("failed: {}", format_reason_for_summary(&reason)),
                None => match fallback_reason.filter(|r| !r.is_empty()) {
                    Some(reason) => format!("failed: {}", format_reason_for_summary(reason)),
                    None => "failed".to_string(),
                },
            },
        };
        let run_ts = state
            .pointer("/runtime/last_run/completed_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(LogLine {
            ts: run_ts,
            text: format!("■ run {status_text}{totals_text}"),
            kind: LogKind::RunEnd,
            path: None,
        })
    }

    /// Diffs `state` against the previous snapshot this instance was
    /// given, returning every new log line, sorted by the underlying
    /// event's own timestamp.
    pub fn diff(&mut self, state: &Value, plan: &PlanTree) -> Vec<LogLine> {
        let mut flat = flatten_state(state);
        let mut lines = Vec::new();
        let mut held_this_poll: Vec<String> = Vec::new();
        // `drain_until_ended` stops polling at the first tick that
        // produces no lines, then diffs the final `state.json` once
        // more -- a hold started on that last diff, or on the last
        // live tick before it, would never get the follow-up poll
        // that resolves it, losing the line for good rather than just
        // delaying it. The final diff runs after events are fully
        // drained, so a real named report's path is already in
        // `report_paths` by then; skipping the hold here only ever
        // lets a true first sighting through immediately, never a
        // report `report_only` above would otherwise have caught.
        let run_has_ended = run_ended(state);

        for (path, node) in &flat {
            // #610: a named report writes a state node (#605), but a
            // report never fires start/end, so this must never grow
            // its own ▶/✓/✗ line here -- `report_line` (from
            // `diff_event`'s own `Event::Report` branch) is its one
            // and only log line. With a plan, `PlanTree::is_report_path`
            // resolves this exactly; where the plan has no real entry
            // at this path at all (`osi watch`, a `use: inline:`/
            // `ref:` child), the paths `diff_event` has seen a report
            // on stand in instead, same as `RunModel::observe`'s own
            // fallback -- gated the same way, by whether the plan
            // already has something to say about this exact path, and
            // by the node's own value looking like a report, so a
            // real container sharing the path isn't hidden by it.
            let report_only = plan.is_report_path(path)
                || (plan.match_path(path).is_none_or(|m| m.entries.is_empty())
                    && self.report_paths.contains(path)
                    && crate::render::is_report_value(&node.value));
            if report_only {
                self.held_once.remove(path);
                continue;
            }
            // #616's own comment: cof writes a named report's state
            // node just before it fires the report event, so a poll
            // landing between the two sees a node already shaped like
            // one with nothing in `report_paths` yet to tell it apart
            // from a real step -- printing a step line for it then
            // can't be taken back once the event does arrive. Held
            // for exactly one poll: a path shaped this way, seen for
            // the first time (`self.last` has nothing for it yet), is
            // skipped this round, and `held_this_poll` keeps it out of
            // `self.last` below so the next poll re-enters here with
            // the same choice -- the event resolves it (`report_only`
            // above now matches) or, `held_once.remove` now finding it
            // already there, it falls through to the ordinary first-
            // sighting lines below instead of holding a second time.
            let unconfirmed_reportish = !run_has_ended
                && plan.match_path(path).is_none_or(|m| m.entries.is_empty())
                && !self.last.contains_key(path)
                && crate::render::is_report_value(&node.value);
            if unconfirmed_reportish && !self.held_once.remove(path) {
                self.held_once.insert(path.clone());
                held_this_poll.push(path.clone());
                continue;
            }
            let is_container = is_container(&node.meta);
            let event_sourced = self.event_sourced.contains(path);
            match self.last.get(path) {
                None => {
                    if is_container {
                        // A container gets no ▶/✓/✗ of its own, but a
                        // named `if`'s `meta.branch` is already set
                        // before its body even starts (DESIGN.md
                        // §1.2), and a loop's `meta.progress` can
                        // already be non-trivial the first time osi
                        // observes it at all (a fast first pass, or
                        // attaching mid-run) — both must still be
                        // checked on first sight, not only against a
                        // `prev` this branch never reaches (F9).
                        if let Some(branch) = &node.branch {
                            lines.push(LogLine {
                                ts: node.created_at.clone(),
                                text: format!("◆ {path} → {branch}"),
                                kind: LogKind::Decision,
                                path: Some(path.clone()),
                            });
                        }
                        if let (Some(done), Some(total)) = (node.progress_done, node.progress_total)
                        {
                            let eta = node
                                .progress_eta_s
                                .map(crate::format::format_duration)
                                .unwrap_or_else(|| "?".to_string());
                            lines.push(LogLine {
                                ts: node
                                    .completed_at
                                    .clone()
                                    .or_else(|| node.created_at.clone()),
                                text: format!("⟳ {path} pass {done}/{total}  ETA {eta}"),
                                kind: LogKind::Progress,
                                path: Some(path.clone()),
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
                            kind: LogKind::Start,
                            path: Some(path.clone()),
                        });
                    } else {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("▶ {path}  {}", summary_for(node, path, plan)),
                            kind: LogKind::Start,
                            path: Some(path.clone()),
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
                                    kind: LogKind::Start,
                                    path: Some(path.clone()),
                                });
                                lines.push(end_line_numbered(path, node, plan, *pass));
                            } else if prev.completed_at != node.completed_at {
                                // Same created_at, a different completion: the
                                // previous sighting was a failed attempt that
                                // got retried, not this pass's own end (#421 —
                                // created_at no longer moves between a pass's
                                // own attempts, so the earlier ✗ this crate
                                // already printed for that sighting needs a
                                // retroactive ↻ alongside this pass's real end).
                                lines.push(LogLine {
                                    ts: prev.completed_at.clone(),
                                    text: format!("↻ {path} retry"),
                                    kind: LogKind::Retry,
                                    path: Some(path.clone()),
                                });
                                lines.push(end_line(path, node, plan));
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
                            kind: LogKind::Retry,
                            path: Some(path.clone()),
                        });
                    }
                    // The current rule (#421): created_at never moves between
                    // a pass's own attempts, so a failed attempt that is about
                    // to retry looks like completed_at going back to running
                    // (null) with created_at unchanged, not a moved created_at.
                    if !is_container
                        && !prev.is_running()
                        && prev.error.is_some()
                        && node.is_running()
                        && prev.created_at == node.created_at
                    {
                        lines.push(LogLine {
                            ts: prev.completed_at.clone(),
                            text: format!("↻ {path} retry"),
                            kind: LogKind::Retry,
                            path: Some(path.clone()),
                        });
                    }

                    if prev.branch.is_none() && node.branch.is_some() {
                        lines.push(LogLine {
                            ts: node.created_at.clone(),
                            text: format!("◆ {path} → {}", node.branch.as_deref().unwrap_or("")),
                            kind: LogKind::Decision,
                            path: Some(path.clone()),
                        });
                    }

                    if prev.progress_done != node.progress_done {
                        if let (Some(done), Some(total)) = (node.progress_done, node.progress_total)
                        {
                            let eta = node
                                .progress_eta_s
                                .map(crate::format::format_duration)
                                .unwrap_or_else(|| "?".to_string());
                            lines.push(LogLine {
                                ts: node
                                    .completed_at
                                    .clone()
                                    .or_else(|| node.created_at.clone()),
                                text: format!("⟳ {path} pass {done}/{total}  ETA {eta}"),
                                kind: LogKind::Progress,
                                path: Some(path.clone()),
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
                        && !any_descendant_failed(
                            path,
                            &flat,
                            &self.event_failed,
                            &self.path_failed_without_id,
                        )
                        && !event_sourced
                    {
                        lines.push(LogLine {
                            ts: node.completed_at.clone(),
                            text: format!(
                                "✗ {path}  {}",
                                truncate_chars(node.error.as_deref().unwrap_or(""), 120)
                            ),
                            kind: LogKind::Failure,
                            path: Some(path.clone()),
                        });
                    }
                }
            }
        }

        for path in &held_this_poll {
            flat.remove(path);
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
    let failed = node.error.is_some();
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
        kind: if failed {
            LogKind::Failure
        } else {
            LogKind::End
        },
        path: Some(path.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_chars_cuts_at_the_first_newline_too() {
        // F13: a multi-line error or tool stdout must not flow into
        // the log as several lines, each breaking the one-line-per-
        // entry shape.
        assert_eq!(truncate_chars("line one\nline two", 120), "line one");
        assert_eq!(truncate_chars("short", 120), "short");
    }

    #[test]
    fn report_line_formats_an_info_report_with_no_data() {
        assert_eq!(
            report_line(ReportLevel::Info, "halfway", None, false),
            "▸ halfway"
        );
    }

    /// #610: a Mustache-rendered message can contain a newline, which
    /// must never flow into the log as a second, untimestamped line.
    #[test]
    fn report_line_collapses_a_multiline_message_to_its_first_line() {
        assert_eq!(
            report_line(ReportLevel::Info, "line one\nline two", None, false),
            "▸ line one"
        );
    }

    #[test]
    fn report_line_caps_a_long_message_at_120_chars() {
        let long_message = "x".repeat(200);
        let line = report_line(ReportLevel::Info, &long_message, None, false);
        let message = line.strip_prefix("▸ ").expect("message prefix");
        assert_eq!(message.chars().count(), REPORT_MESSAGE_LOG_CHARS);
    }

    #[test]
    fn report_line_formats_a_warning_with_a_data_preview() {
        let data = json!({"n": 3});
        assert_eq!(
            report_line(ReportLevel::Warning, "slow", Some(&data), false),
            "▸ warning: slow  {\"n\":3}"
        );
    }

    #[test]
    fn report_line_shows_data_truncated_instead_of_a_preview() {
        let data = json!({"n": 3});
        assert_eq!(
            report_line(ReportLevel::Info, "big", Some(&data), true),
            "▸ big  (data truncated)"
        );
    }

    #[test]
    fn report_line_caps_the_data_preview_at_200_chars_with_an_ellipsis() {
        let long_value = "x".repeat(250);
        let data = json!({ "v": long_value });
        let line = report_line(ReportLevel::Info, "m", Some(&data), false);
        let preview = line.strip_prefix("▸ m  ").expect("message prefix");
        assert_eq!(
            preview.chars().count(),
            REPORT_DATA_LOG_CHARS + 1,
            "{preview}"
        );
        assert!(preview.ends_with('…'), "{preview}");
    }

    #[test]
    fn diff_event_turns_a_report_event_into_its_own_log_line() {
        let mut differ = Differ::new();
        let event = Event::Report {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime".to_string(),
            level: ReportLevel::Warning,
            message: "slow progress".to_string(),
            data: None,
            data_truncated: false,
        };
        let model = RunModel::new();
        let lines = differ.diff_event(&event, &PlanTree::empty(), &model);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "▸ warning: slow progress");
        assert_eq!(lines[0].ts.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn diff_event_turns_a_run_start_event_into_its_own_log_line_naming_the_run_id() {
        let mut differ = Differ::new();
        let event = Event::RunStart {
            ts: "t".to_string(),
            run_id: "abc123".to_string(),
            pid: Some(1),
        };
        let model = RunModel::new();
        let lines = differ.diff_event(&event, &PlanTree::empty(), &model);
        assert_eq!(
            lines,
            vec![LogLine {
                ts: Some("t".to_string()),
                text: "■ run started  abc123".to_string(),
                kind: LogKind::RunStart,
                path: None,
            }]
        );
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
    /// prompt) — but with a real plan, osi can and should say
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

    /// #610: a named report's own state node must never grow its
    /// own `▶`/`✓` line in `diff`'s state walk -- `report_line`
    /// (from `diff_event`'s own `Event::Report` branch) is its one
    /// and only log line. Exercised with no plan at all (`osi
    /// watch`'s own shape): the one case `diff` has no `PlanTree::
    /// is_report_path` to lean on, only the paths `diff_event` has
    /// itself seen a report on.
    #[test]
    fn a_named_reports_state_node_never_gets_a_start_or_done_line() {
        let mut differ = Differ::new();
        differ.diff_event(
            &Event::Report {
                ts: "t0".to_string(),
                path: "prime.status".to_string(),
                level: ReportLevel::Info,
                message: "on track".to_string(),
                data: None,
                data_truncated: false,
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t0"},
            "status": {"value": {"message": "on track", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t0"}}
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines.iter().all(|l| !l.text.contains("prime.status")),
            "a named report must never grow its own step line: {lines:?}"
        );
    }

    /// #616's own comment: a live-state write can land between `cof`
    /// writing a named report's own state node and firing its report
    /// event -- a poll in that window must hold rather than print a
    /// step line it can never take back. Held for exactly one poll:
    /// the event arrives before the second `diff`, so the node is
    /// recognized as a report (via `report_paths`, same as when the
    /// event always arrives first) and still never gets one.
    #[test]
    fn an_unconfirmed_reportish_node_is_held_one_poll_then_recognized_once_its_event_arrives() {
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t0"},
            "status": {"value": {"message": "on track", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t0"}}
        }});

        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines.iter().all(|l| !l.text.contains("prime.status")),
            "an unconfirmed report-shaped node must be held, not printed: {lines:?}"
        );

        differ.diff_event(
            &Event::Report {
                ts: "t0".to_string(),
                path: "prime.status".to_string(),
                level: ReportLevel::Info,
                message: "on track".to_string(),
                data: None,
                data_truncated: false,
            },
            &PlanTree::empty(),
            &RunModel::new(),
        );

        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines.iter().all(|l| !l.text.contains("prime.status")),
            "a report confirmed after being held must still never get a step line: {lines:?}"
        );
    }

    /// The other half of the same hold (#616's comment): a tool whose
    /// own output merely happens to match the report shape, with no
    /// report event ever arriving for it, still gets its ordinary
    /// `▶`/`✓` lines -- delayed by exactly one poll, never lost.
    #[test]
    fn an_unconfirmed_reportish_node_still_gets_its_step_lines_if_no_event_ever_arrives() {
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t0"},
            "looks_like_a_report": {
                "value": {"message": "tool output", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t0", "provider": "shell", "stdout": "whatever"}
            }
        }});

        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines
                .iter()
                .all(|l| !l.text.contains("prime.looks_like_a_report")),
            "held on the first poll: {lines:?}"
        );

        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines
                .iter()
                .any(|l| l.text.starts_with("▶ prime.looks_like_a_report")),
            "printed as an ordinary first sighting once the hold expires: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.text.starts_with("✓ prime.looks_like_a_report")),
            "printed as an ordinary first sighting once the hold expires: {lines:?}"
        );
    }

    /// The hold exists to delay a line, never to lose one --
    /// `drain_until_ended` (`oscilloscope`'s `main.rs`) stops polling
    /// at the first tick with no new lines, then diffs the final
    /// `state.json` once more, so a hold started on *that* diff would
    /// never get the follow-up poll that resolves it. Once
    /// `runtime.last_run.completed_at` is set, a report-shaped node
    /// with no event must print its step lines on this very diff
    /// instead of holding.
    #[test]
    fn an_unconfirmed_reportish_node_is_not_held_once_the_run_has_ended() {
        let mut differ = Differ::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1"},
                "looks_like_a_report": {
                    "value": {"message": "tool output", "level": "info", "data": null},
                    "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell", "stdout": "whatever"}
                }
            }
        });

        let lines = differ.diff(&state, &PlanTree::empty());
        assert!(
            lines
                .iter()
                .any(|l| l.text.starts_with("▶ prime.looks_like_a_report")),
            "a run-ended diff must never hold: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.text.starts_with("✓ prime.looks_like_a_report")),
            "a run-ended diff must never hold: {lines:?}"
        );
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
                                prompt_file: None,
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
                                prompt_file: None,
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
    fn container_failure_seen_as_a_snapshot_before_its_end_event_prints_once() {
        // F1: once `prime`'s own `start` has fired, it's event-sourced
        // for the rest of the run, so `diff`'s own container-failure
        // branch (P2-6) must defer to the matching `end` event even
        // though the state snapshot sees the failure first -- real
        // streams always start a path with its own `start` (DESIGN.md
        // §1.4) before anything else can observe it.
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

        let running =
            json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"}}});
        differ.diff(&running, &plan);

        let failed = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)", "flow": "chain"}}});
        let snapshot_lines = differ.diff(&failed, &plan);
        assert!(
            snapshot_lines.is_empty(),
            "an event-sourced container must leave its own ✗ to the event: {snapshot_lines:?}"
        );

        let event_lines = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(0),
                path: "prime".to_string(),
                ok: false,
                ms: Some(1),
                error: Some("Interrupted (Ctrl-C/SIGINT)".to_string()),
            },
            &plan,
            &model,
        );
        assert_eq!(
            event_lines
                .iter()
                .filter(|l| l.text.starts_with("✗ prime "))
                .count(),
            1,
            "{event_lines:?}"
        );
    }

    #[test]
    fn container_failure_seen_as_a_snapshot_after_its_end_event_prints_once() {
        // The reverse order of the previous test: the `end` event is
        // tailed before the next state poll lands. The later
        // snapshot, even though it would be the first time `diff`
        // itself sees the failure, must still defer to the event
        // because `prime` is already event-sourced.
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
        let running =
            json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"}}});
        differ.diff(&running, &plan);

        let event_lines = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(0),
                path: "prime".to_string(),
                ok: false,
                ms: Some(1),
                error: Some("Interrupted (Ctrl-C/SIGINT)".to_string()),
            },
            &plan,
            &model,
        );
        assert_eq!(
            event_lines
                .iter()
                .filter(|l| l.text.starts_with("✗ prime "))
                .count(),
            1,
            "{event_lines:?}"
        );

        let failed = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)", "flow": "chain"}}});
        let snapshot_lines = differ.diff(&failed, &plan);
        assert!(
            snapshot_lines.is_empty(),
            "an event-sourced container must leave its own ✗ to the event: {snapshot_lines:?}"
        );
    }

    #[test]
    fn container_failure_start_and_end_before_the_first_snapshot_in_the_same_tick_prints_once() {
        // Mirrors `main.rs`'s own `drain_observations`: a tick feeds
        // every newly tailed event to `diff_event` first, then runs
        // `diff` once against that same poll's state snapshot. Here
        // the path's very first state observation is already the
        // failed one -- `diff`'s container branch never even reaches
        // its `Some(prev)` arm for it, so the ✗ can only ever come
        // from the `end` event.
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        let mut tick_lines = differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(0),
                path: "prime".to_string(),
            },
            &plan,
            &model,
        );
        tick_lines.extend(differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(0),
                path: "prime".to_string(),
                ok: false,
                ms: Some(1),
                error: Some("Interrupted (Ctrl-C/SIGINT)".to_string()),
            },
            &plan,
            &model,
        ));
        let failed = json!({"prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)", "flow": "chain"}}});
        tick_lines.extend(differ.diff(&failed, &plan));

        assert_eq!(
            tick_lines
                .iter()
                .filter(|l| l.text.starts_with("✗ prime "))
                .count(),
            1,
            "{tick_lines:?}"
        );
    }

    #[test]
    fn container_failure_in_a_reused_loop_path_prints_once_per_pass() {
        // LOOP REGRESSION: a chain loop's body reuses the same
        // container path every pass (DESIGN.md §2.3, the container
        // analogue of `unnamed_pass_counts` for leaves). cof emits one
        // `end` event with `ok: false` per pass for a body that fails
        // on its own each time (tests/conformance/cases/on-error-if/
        // expected.events.jsonl), so osi must print one ✗ per pass
        // too -- a path-keyed "already reported" set would wrongly
        // collapse the second pass's ✗ into nothing.
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let path = "prime.loop.gate";

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(0),
                path: path.to_string(),
            },
            &plan,
            &model,
        );
        let running1 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "mode": "cel"}}
            }
        }});
        differ.diff(&running1, &plan);

        let pass1 = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(0),
                path: path.to_string(),
                ok: false,
                ms: Some(1),
                error: Some("condition error".to_string()),
            },
            &plan,
            &model,
        );
        let failed1 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "mode": "cel", "error": "condition error"}}
            }
        }});
        differ.diff(&failed1, &plan);

        differ.diff_event(
            &Event::Start {
                ts: "t2".to_string(),
                id: Some(1),
                path: path.to_string(),
            },
            &plan,
            &model,
        );
        let running2 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t2", "completed_at": null, "mode": "cel"}}
            }
        }});
        differ.diff(&running2, &plan);

        let pass2 = differ.diff_event(
            &Event::End {
                ts: "t3".to_string(),
                id: Some(1),
                path: path.to_string(),
                ok: false,
                ms: Some(1),
                error: Some("condition error".to_string()),
            },
            &plan,
            &model,
        );
        let failed2 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t2", "completed_at": "t3", "mode": "cel", "error": "condition error"}}
            }
        }});
        let tail_lines = differ.diff(&failed2, &plan);

        assert_eq!(
            pass1
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.loop.gate "))
                .count(),
            1,
            "{pass1:?}"
        );
        assert_eq!(
            pass2
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.loop.gate "))
                .count(),
            1,
            "{pass2:?}"
        );
        assert!(
            tail_lines.is_empty(),
            "the event-sourced container's second failed snapshot must print nothing new: {tail_lines:?}"
        );
    }

    #[test]
    fn container_failure_in_a_reused_loop_path_without_events_prints_once_per_pass() {
        // The state-only counterpart of the previous test (an engine
        // with no `--events`, or a state-only `osi watch`): `diff`'s
        // own branch must still print one ✗ per pass, exactly as main
        // always has, with no event ever marking the path as
        // event-sourced.
        let mut differ = Differ::new();
        let plan = PlanTree::empty();

        let running1 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "mode": "cel"}}
            }
        }});
        differ.diff(&running1, &plan);

        let failed1 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "mode": "cel", "error": "condition error"}}
            }
        }});
        let pass1 = differ.diff(&failed1, &plan);

        let running2 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t2", "completed_at": null, "mode": "cel"}}
            }
        }});
        differ.diff(&running2, &plan);

        let failed2 = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "loop": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
                "gate": {"value": null, "meta": {"created_at": "t2", "completed_at": "t3", "mode": "cel", "error": "condition error"}}
            }
        }});
        let pass2 = differ.diff(&failed2, &plan);

        assert_eq!(
            pass1
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.loop.gate "))
                .count(),
            1,
            "{pass1:?}"
        );
        assert_eq!(
            pass2
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.loop.gate "))
                .count(),
            1,
            "{pass2:?}"
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
    fn finish_reports_an_interrupted_run_as_cancelled_even_with_an_open_event_sourced_path() {
        // M1: there is no `open_event_paths` gate left to release --
        // an effect interrupted mid-flight never gets its own `end`
        // event at all, but that no longer matters, since the caller
        // decides when to call `finish`, from a state it already
        // knows is final. The run itself was cancelled, not failed
        // (DESIGN.md §2.1 rule 7): `prime.meta.error` starts with
        // "Interrupted".
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
        assert!(
            line.text
                .starts_with("■ run cancelled: Interrupted (Ctrl-C/SIGINT)"),
            "{line:?}"
        );
    }

    #[test]
    fn finish_reports_a_real_failure_distinctly_from_a_cancelled_run() {
        // A regression: an ended run whose error
        // does *not* start with "Interrupted" must still read
        // "failed", never "cancelled".
        let mut differ = Differ::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": false, "meta": {"completed_at": "t1", "error": "prime.a: /bin/ls failed (exit 1): ls: no"}}
        });
        let line = differ.finish(&state, None).expect("a summary line");
        assert!(
            line.text
                .starts_with("■ run failed: prime.a: /bin/ls failed"),
            "{line:?}"
        );
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
    fn a_reused_paths_earlier_failure_is_never_forgotten_even_once_a_later_pass_succeeds() {
        // Review finding 5 on #458's comment 4: `event_failed` used to
        // clear a path's own entry on its next `start`, so the
        // container below (`an_unnamed_tree_loops_reused_pass_failure_
        // does_not_get_a_redundant_cross_mark_once_a_later_pass_
        // succeeds`) printed a second, redundant ✗ once a later pass's
        // own success cleared pass 1's own already-reported one. Kept
        // per instance (by id) instead, this is the deliberate
        // trade-off: a CEL error on the *same* container, in a *later*
        // pass where the reused leaf itself succeeds, is now wrongly
        // read as still being that old, unrelated failure bubbling up
        // -- accepted, since the redundant-✗ case below is both more
        // common and more visibly wrong.
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
        // Pass 1: the unnamed loop's leaf fails.
        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.u_loop.leaf".to_string(),
            },
            &plan,
            &model,
        );
        differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.u_loop.leaf".to_string(),
                ok: false,
                ms: Some(1),
                error: Some("boom".to_string()),
            },
            &plan,
            &model,
        );
        // Pass 2 reuses the same path (DESIGN.md §2.3) and succeeds.
        differ.diff_event(
            &Event::Start {
                ts: "t2".to_string(),
                id: Some(2),
                path: "prime.u_loop.leaf".to_string(),
            },
            &plan,
            &model,
        );
        differ.diff_event(
            &Event::End {
                ts: "t3".to_string(),
                id: Some(2),
                path: "prime.u_loop.leaf".to_string(),
                ok: true,
                ms: Some(1),
                error: None,
            },
            &plan,
            &model,
        );
        // The container itself now fails on its own, in the same
        // batch as nothing else failing.
        let container_lines = differ.diff_event(
            &Event::End {
                ts: "t3".to_string(),
                id: Some(0),
                path: "prime".to_string(),
                ok: false,
                ms: Some(5),
                error: Some("loop condition: CEL error".to_string()),
            },
            &plan,
            &model,
        );
        assert!(
            container_lines.is_empty(),
            "pass 1's own, long-settled failure is still remembered, so the container's own (here, genuinely unrelated) failure is still read as that one bubbling up: {container_lines:?}"
        );
    }

    #[test]
    fn an_unnamed_tree_loops_reused_pass_failure_does_not_get_a_redundant_cross_mark_once_a_later_pass_succeeds()
     {
        // Review finding 5 on #458's comment 4: an unnamed tree loop
        // whose passes exceed `max_concurrency` reuses the same leaf
        // path for every pass (DESIGN.md §2.3). Pass 1 at that path
        // fails and gets its own ✗ immediately. With
        // `stop_on_error: continue`, later passes keep running at the
        // very same path -- pass 2 succeeds -- but the loop's own
        // container still ends with `ok: false` overall, since *some*
        // pass failed. That end must not print a second, redundant ✗
        // for the exact same failure pass 1's own line already
        // reported.
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
                path: "prime.nap".to_string(),
            },
            &plan,
            &model,
        );
        let pass1_lines = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.nap".to_string(),
                ok: false,
                ms: Some(1),
                error: Some("boom".to_string()),
            },
            &plan,
            &model,
        );
        differ.diff_event(
            &Event::Start {
                ts: "t2".to_string(),
                id: Some(2),
                path: "prime.nap".to_string(),
            },
            &plan,
            &model,
        );
        differ.diff_event(
            &Event::End {
                ts: "t3".to_string(),
                id: Some(2),
                path: "prime.nap".to_string(),
                ok: true,
                ms: Some(1),
                error: None,
            },
            &plan,
            &model,
        );
        let container_lines = differ.diff_event(
            &Event::End {
                ts: "t3".to_string(),
                id: Some(0),
                path: "prime".to_string(),
                ok: false,
                ms: Some(5),
                error: Some("prime.nap: boom".to_string()),
            },
            &plan,
            &model,
        );
        assert!(
            pass1_lines
                .iter()
                .any(|l| l.text.starts_with("✗ prime.nap")),
            "{pass1_lines:?}"
        );
        assert!(
            container_lines.is_empty(),
            "the container must not print a second, redundant cross mark for a failure pass 1 already reported: {container_lines:?}"
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
        // F9: `meta.branch` is already set the very first time osi
        // observes a named `if` at all (DESIGN.md §1.2), so there is no
        // earlier `prev` without a branch to compare against.
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "gate": {"value": null, "meta": {"completed_at": null, "mode": "cel", "branch": "then"},
                "chosen": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
            }
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        let decision = lines
            .iter()
            .find(|l| l.text == "◆ prime.gate → then")
            .unwrap_or_else(|| panic!("{lines:?}"));
        assert_eq!(decision.kind, LogKind::Decision);
        assert_eq!(decision.path.as_deref(), Some("prime.gate"));
    }

    /// Issue #635: every `LogLine` carries a `kind`/`path` the story
    /// can classify without reparsing `text`'s own glyph, but `--log`
    /// itself still prints only `ts`/`text` (`print_line` in osi's
    /// own `main.rs`), so this never changes `--log`'s own output.
    #[test]
    fn log_lines_carry_their_own_kind_and_path_for_start_end_and_failure() {
        let mut differ = Differ::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let lines = differ.diff(&state, &PlanTree::empty());
        let start = lines.iter().find(|l| l.text.starts_with('▶')).unwrap();
        assert_eq!(start.kind, LogKind::Start);
        assert_eq!(start.path.as_deref(), Some("prime.a"));
        let end = lines.iter().find(|l| l.text.starts_with('✓')).unwrap();
        assert_eq!(end.kind, LogKind::End);
        assert_eq!(end.path.as_deref(), Some("prime.a"));

        let mut differ2 = Differ::new();
        let failing = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
        }});
        let fail_lines = differ2.diff(&failing, &PlanTree::empty());
        let failure = fail_lines.iter().find(|l| l.text.starts_with('✗')).unwrap();
        assert_eq!(failure.kind, LogKind::Failure);
        assert_eq!(failure.path.as_deref(), Some("prime.a"));
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
    fn a_leaf_that_restarts_with_the_same_created_at_gets_a_retry_line_before_running() {
        // #421: created_at no longer moves between a pass's own
        // attempts, so the transition back to running (completed_at
        // null, created_at unchanged) after a failed attempt is now
        // the only state-only signal that attempt is a retry, not a
        // moved created_at (the test above, kept for whichever
        // engine/version still moves it).
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&running, &PlanTree::empty());

        let failed = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
        }});
        let failed_lines = differ.diff(&failed, &PlanTree::empty());
        assert!(
            failed_lines
                .iter()
                .any(|l| l.text.starts_with("✗ prime.flaky")),
            "{failed_lines:?}"
        );

        let retrying = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let lines = differ.diff(&retrying, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text == "↻ prime.flaky retry"),
            "{lines:?}"
        );

        let succeeded = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t2", "error": null, "provider": "shell"}}
        }});
        let final_lines = differ.diff(&succeeded, &PlanTree::empty());
        assert!(
            final_lines
                .iter()
                .any(|l| l.text.starts_with("✓ prime.flaky")),
            "{final_lines:?}"
        );
    }

    #[test]
    fn a_leaf_that_succeeds_with_the_same_created_at_as_an_earlier_failure_gets_a_retroactive_retry_line()
     {
        // No snapshot landed during the retried attempt itself -- the
        // only two sightings are the first failure and the eventual
        // success, both with the same created_at (#421). The ✗ this
        // crate already printed for the failure stands; the success
        // still needs its own ✓ plus a ↻ marking that failure as a
        // retry rather than this pass's own end.
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&running, &PlanTree::empty());

        let failed = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
        }});
        differ.diff(&failed, &PlanTree::empty());

        let succeeded = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t2", "error": null, "provider": "shell"}}
        }});
        let lines = differ.diff(&succeeded, &PlanTree::empty());
        assert!(
            lines.iter().any(|l| l.text == "↻ prime.flaky retry"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.text.starts_with("✓ prime.flaky")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_leaf_already_failed_for_good_gets_no_duplicate_end_line_on_a_repeat_poll() {
        // Exhausted retries: the same terminal failure polled twice in
        // a row (completed_at unchanged) must not be mistaken for a
        // hidden retry-then-end -- there was no hidden attempt, just a
        // repeat snapshot of the same already-reported end.
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&running, &PlanTree::empty());

        let failed = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
        }});
        let first_fail_lines = differ.diff(&failed, &PlanTree::empty());
        assert_eq!(
            first_fail_lines
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.flaky"))
                .count(),
            1,
            "{first_fail_lines:?}"
        );

        let repeat_poll = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
        }});
        let lines = differ.diff(&repeat_poll, &PlanTree::empty());
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn a_leaf_that_fails_again_after_a_retry_gets_one_retry_line_and_one_final_cross() {
        // #421: a retry that itself fails, exhausting the budget, must
        // still produce exactly one final ✗ -- the earlier attempt's ✗
        // (printed when it first failed) gets annotated as a retry via
        // the ↻ line, not duplicated into a second ✗ for the same pass.
        let mut differ = Differ::new();
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        differ.diff(&running, &PlanTree::empty());

        let first_failure = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "boom", "provider": "shell"}}
        }});
        let first_fail_lines = differ.diff(&first_failure, &PlanTree::empty());
        assert_eq!(
            first_fail_lines
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.flaky"))
                .count(),
            1,
            "{first_fail_lines:?}"
        );

        let retrying = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let retry_lines = differ.diff(&retrying, &PlanTree::empty());
        assert!(
            retry_lines.iter().any(|l| l.text == "↻ prime.flaky retry"),
            "{retry_lines:?}"
        );

        let exhausted = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "flaky": {"value": null, "meta": {"created_at": "t0", "completed_at": "t2", "error": "boom again", "provider": "shell"}}
        }});
        let final_lines = differ.diff(&exhausted, &PlanTree::empty());
        assert_eq!(
            final_lines
                .iter()
                .filter(|l| l.text.starts_with("✗ prime.flaky"))
                .count(),
            1,
            "{final_lines:?}"
        );
    }

    #[test]
    fn diff_event_detects_a_container_from_a_just_polled_preview_with_no_plan_and_no_prior_last() {
        // Issue #433: `osi watch` with no `--plan` has no static plan
        // to check, so a container whose first-ever state write lands
        // in the very same tick as its own `start` event must still
        // be recognised from that fresh snapshot, not misread as a
        // leaf just because `diff` (which only updates `last`) hasn't
        // been handed it yet this tick.
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let fresh_state = json!({
            "prime": {
                "value": null,
                "meta": {"completed_at": null},
                "gate": {"value": null, "meta": {"completed_at": null, "mode": "cel"}}
            }
        });
        differ.note_preview(&fresh_state);
        let lines = differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.gate".to_string(),
            },
            &plan,
            &model,
        );
        assert!(
            lines.is_empty(),
            "a container detected from the preview must get no ▶ line: {lines:?}"
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

        // State already knows this path by the time its `end` arrives
        // (unlike #545's race) -- `event_sourced` from the `start`
        // above already makes `diff` itself skip it, so this only
        // primes `last` for the immediate-resolve path below.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "tree"},
            "fan": {"value": null, "meta": {"completed_at": null, "flow": "tree"},
                "a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"}}
            }
        }});
        assert!(differ.diff(&state, &PlanTree::empty()).is_empty());

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

    /// #545, arrival order 1: the `end` event arrives before any state
    /// poll has this path at all -- both engines emit it before their
    /// own live-state write for the same leaf, so this is the common
    /// case, not a corner one. The line must wait rather than guess
    /// "ok", and must carry the real token summary once a later state
    /// poll (here, `diff`) catches up.
    #[test]
    fn an_end_event_before_its_tokens_are_known_is_held_until_state_catches_up() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
            },
            &plan,
            &model,
        );
        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
                ok: true,
                ms: Some(100),
                error: None,
            },
            &plan,
            &model,
        );
        assert!(end.is_empty(), "held, not guessed: {end:?}");
        assert!(differ.has_pending_ends());

        // One tick still with nothing new: still held, not yet at the
        // wait bound.
        assert!(differ.settle_pending_ends(Instant::now()).is_empty());
        assert!(differ.has_pending_ends());

        // The state poll that finally has it -- `prime.ask` is
        // already event-sourced, so `diff` itself emits nothing for
        // it; this only updates `last` for `settle_pending_ends` to
        // find.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": "hi", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 3, "tokens_received": 5}}
        }});
        assert!(differ.diff(&state, &plan).is_empty());

        let settled = differ.settle_pending_ends(Instant::now());
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].text, "✓ prime.ask  0.1s  ↑3 t ↓5 t");
        assert_eq!(settled[0].ts.as_deref(), Some("t1"));
        assert!(!differ.has_pending_ends());
    }

    /// #545, review finding 1: the live-state writer can catch a leaf
    /// mid-flight, with a node already in state but not yet complete
    /// (`completed_at: null`, no tokens) -- the `end` event must still
    /// hold, not resolve from that running node, or it prints `ok`
    /// immediately, the very race this whole mechanism exists to
    /// avoid. The line comes out with real tokens once a later poll
    /// shows the node complete.
    #[test]
    fn an_end_event_whose_state_is_still_running_is_held_not_resolved_as_ok() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
            },
            &plan,
            &model,
        );

        // A live-state write lands while `prime.ask` is still running
        // (DESIGN.md §1.4: a chain leaf's running node is visible
        // "only by accident", caught by a write that happens to land
        // mid-flight).
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "tokens_sent": null, "tokens_received": null}}
        }});
        assert!(differ.diff(&running, &plan).is_empty());

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
                ok: true,
                ms: Some(100),
                error: None,
            },
            &plan,
            &model,
        );
        assert!(
            end.is_empty(),
            "a running node must not resolve the end as ok: {end:?}"
        );
        assert!(differ.has_pending_ends());

        let completed = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": "hi", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 3, "tokens_received": 5}}
        }});
        assert!(differ.diff(&completed, &plan).is_empty());

        let settled = differ.settle_pending_ends(Instant::now());
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].text, "✓ prime.ask  0.1s  ↑3 t ↓5 t");
        assert!(!differ.has_pending_ends());
    }

    /// #545, review finding 1's second gap: `known_node` must look at
    /// `preview` (this tick's own fresh poll) before `last` (a tick
    /// old), not the other way around -- `note_preview` always runs
    /// ahead of `diff` within the same tick (`main.rs`'s
    /// `observe_tick`), so by the time `diff_event` processes an
    /// `end`, `preview` can already hold the completed node while
    /// `last` still holds that same path's own stale, running one.
    #[test]
    fn known_node_prefers_the_fresher_preview_over_a_stale_running_last() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
            },
            &plan,
            &model,
        );
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "tokens_sent": null, "tokens_received": null}}
        }});
        // `diff` commits the still-running node into `last`.
        assert!(differ.diff(&running, &plan).is_empty());

        // This tick's own fresh poll already has the node complete,
        // handed to `note_preview` ahead of `diff` ever seeing it.
        let completed = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": "hi", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 3, "tokens_received": 5}}
        }});
        differ.note_preview(&completed);

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
                ok: true,
                ms: Some(100),
                error: None,
            },
            &plan,
            &model,
        );
        assert_eq!(end.len(), 1, "{end:?}");
        assert_eq!(end[0].text, "✓ prime.ask  0.1s  ↑3 t ↓5 t");
        assert!(!differ.has_pending_ends());
    }

    /// An `end` event's `ms` over a minute goes through
    /// `format_duration` like every other duration, not the log's
    /// own `{:.1}s` -- a 47-minute, 57.3s step prints `47m57s`, not
    /// the un-carried `2877.3s`.
    #[test]
    fn end_event_duration_over_a_minute_carries_into_minutes() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.slow".to_string(),
            },
            &plan,
            &model,
        );
        let completed = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "slow": {"value": "done", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        assert!(differ.diff(&completed, &plan).is_empty());

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.slow".to_string(),
                ok: true,
                ms: Some(2_877_300),
                error: None,
            },
            &plan,
            &model,
        );
        assert_eq!(end.len(), 1, "{end:?}");
        assert_eq!(end[0].text, "✓ prime.slow  47m57s  ok");
    }

    /// #545, review finding 7: the `preview` fallback in `known_node`
    /// also has to serve an `end` arriving with no `last` entry at
    /// all, `preview` the only place the completed node has ever been
    /// seen.
    #[test]
    fn known_node_resolves_from_preview_alone_with_no_last_entry() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
            },
            &plan,
            &model,
        );
        let completed = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": "hi", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 3, "tokens_received": 5}}
        }});
        differ.note_preview(&completed);

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
                ok: true,
                ms: Some(100),
                error: None,
            },
            &plan,
            &model,
        );
        assert_eq!(end.len(), 1, "{end:?}");
        assert_eq!(end[0].text, "✓ prime.ask  0.1s  ↑3 t ↓5 t");
        assert!(!differ.has_pending_ends());
    }

    /// #545, arrival order 2 (the reverse): the state poll that has
    /// this path's tokens lands *before* `diff_event` ever processes
    /// the matching `end` -- the common case once the race is lost
    /// the other way, e.g. a state poll and the events tailer both
    /// drained in the same tick, state first. No hold needed at all:
    /// `known_node` already has it.
    #[test]
    fn an_end_event_with_its_tokens_already_known_resolves_immediately() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
            },
            &plan,
            &model,
        );
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null, "flow": "chain"},
            "ask": {"value": "hi", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 3, "tokens_received": 5}}
        }});
        assert!(differ.diff(&state, &plan).is_empty());

        let end = differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
                ok: true,
                ms: Some(100),
                error: None,
            },
            &plan,
            &model,
        );
        assert_eq!(end.len(), 1);
        assert!(
            end[0].text.starts_with("✓ prime.ask  0.1s  ↑3 t ↓5 t"),
            "{end:?}"
        );
        assert!(!differ.has_pending_ends());
    }

    /// #545's own bound: a held line whose state never catches up
    /// within `END_SUMMARY_MAX_WAIT` of wall-clock time falls back to
    /// the old `ok` text rather than holding for the rest of the run.
    /// Built instants, not a real sleep: just under the bound still
    /// holds, right at it resolves.
    #[test]
    fn an_end_event_still_unresolved_past_the_wait_bound_falls_back_to_ok() {
        let mut differ = Differ::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();

        differ.diff_event(
            &Event::Start {
                ts: "t0".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
            },
            &plan,
            &model,
        );
        let before = Instant::now();
        differ.diff_event(
            &Event::End {
                ts: "t1".to_string(),
                id: Some(1),
                path: "prime.ask".to_string(),
                ok: true,
                ms: Some(100),
                error: None,
            },
            &plan,
            &model,
        );
        let after = Instant::now();

        // `before`/`after` bracket the real `held_at` the push above
        // recorded internally, so each bound below holds regardless
        // of exactly when within that bracket it landed: a `now` only
        // `END_SUMMARY_MAX_WAIT` past `before` is still short of the
        // bound measured from the real, later `held_at`; one a full
        // `END_SUMMARY_MAX_WAIT` past `after` is past it, measured
        // from the real, earlier `held_at`.
        assert!(
            differ
                .settle_pending_ends(before + END_SUMMARY_MAX_WAIT - Duration::from_nanos(1))
                .is_empty()
        );
        assert!(differ.has_pending_ends());

        let settled = differ.settle_pending_ends(after + END_SUMMARY_MAX_WAIT);
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].text, "✓ prime.ask  0.1s  ok");
        assert_eq!(settled[0].ts.as_deref(), Some("t1"));
        assert!(!differ.has_pending_ends());
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
