//! Parses the agent tool's own compact transcript lines
//! (`docs/plugins/agent.md`'s "The transcript's JSONL lines"), already
//! read and bounded by `observe::tail_transcript_lines` -- no file IO
//! here, so every rule below is covered by a plain, `Value`-in test.
//! Issue #637.

use std::collections::BTreeMap;

use serde_json::Value;

/// One `tool_start`/`tool_end` pair, matched by the transcript's own
/// `id` (the agent tool's per-call sequence number) -- `ok`/`ms`/
/// `lines_added`/`lines_removed` stay `None` until (or unless) the
/// matching `tool_end` line has actually arrived, which is exactly
/// how a front end tells a call still running from one that finished
/// (`11:52:30 › bash pytest -q tests/adapters running 25s`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub start_t: Option<String>,
    pub end_t: Option<String>,
    pub tool: String,
    pub target: Option<String>,
    pub ok: Option<bool>,
    pub ms: Option<u64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    /// Set once a later `tool_start`'s own `id` is not greater than
    /// this call's (a retried attempt's own ids start over at 1, same
    /// as the engine's `LiveTracker`) while this call was still
    /// unmatched -- the attempt that started it was killed mid-call,
    /// so no `tool_end` is ever coming for it. Distinct from
    /// [`Self::is_running`]: a reader must never read a stopped call
    /// as still running, nor let it decide `tests`' own outcome.
    pub stopped: bool,
}

impl ToolCall {
    pub fn is_running(&self) -> bool {
        self.ok.is_none() && !self.stopped
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Running,
    Ok,
    Failed,
}

/// Everything the overview's now pane and Flow's selected pane read
/// from one step's transcript tail: its tool calls, oldest first, and
/// the last test command's own outcome (`docs/plugins/agent.md` names
/// no `tests` field -- issue #637's own decision is that osi works
/// this out from the transcript itself).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TranscriptView {
    pub calls: Vec<ToolCall>,
    pub tests: Option<TestOutcome>,
    pub last_reply: Option<String>,
    /// `observe::transcript_tail_truncated`'s own answer for this
    /// read -- the Time screen's zoom pane (issue #650) dims a
    /// leading `…` ahead of the earliest band when this is set,
    /// since the window it tailed doesn't reach back to the
    /// session's own start.
    pub truncated: bool,
}

/// The exact test-runner commands issue #637 names: a `tool_start`
/// whose own `target` contains one of these (a substring match, not
/// a prefix -- `target` can carry a leading `cd ... &&`) makes that
/// call the one `tests` reads from, last one wins.
const TEST_RUNNERS: &[&str] = &[
    "pytest",
    "cargo test",
    "go test",
    "npm test",
    "npx jest",
    "vitest",
    "bun test",
    "mix test",
    "rspec",
    "make test",
];

fn is_test_command(target: &str) -> bool {
    TEST_RUNNERS.iter().any(|needle| target.contains(needle))
}

/// A tool name's own activity glyph (issue #637's `░read ▒edit
/// █bash`, issue #650's zoom-pane bands) -- a substring match on the
/// lower-cased name, so it reads both engines' own casing (pi's
/// lowercase, Claude Code's `Read`/`Edit`/`Bash`) the same way, and
/// still assigns *some* glyph to an MCP tool or any other name
/// neither engine's own built-ins use. The overview's tool strip and
/// the Time screen's zoom pane both read this one classification, so
/// they never disagree about which band a call falls into.
pub fn tool_glyph(tool: &str) -> char {
    let lower = tool.to_ascii_lowercase();
    if lower.contains("edit") || lower.contains("write") {
        '▒'
    } else if lower.contains("bash")
        || lower.contains("run")
        || lower.contains("shell")
        || lower.contains("exec")
    {
        '█'
    } else if lower.contains("read")
        || lower.contains("grep")
        || lower.contains("find")
        || lower.contains("glob")
        || lower.contains("ls")
        || lower.contains("search")
    {
        '░'
    } else {
        '·'
    }
}

/// [`tool_glyph`]'s own classification, as a band kind rather than a
/// glyph -- the Time screen's zoom pane (issue #650) groups a
/// running agent's tool calls into one band per kind, the same four
/// buckets the glyph strip already uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Bash,
    Other,
}

impl ToolKind {
    pub fn classify(tool: &str) -> ToolKind {
        match tool_glyph(tool) {
            '▒' => ToolKind::Edit,
            '█' => ToolKind::Bash,
            '░' => ToolKind::Read,
            _ => ToolKind::Other,
        }
    }
}

/// Builds a [`TranscriptView`] from already-tailed JSONL lines, in the
/// order they appear in the tail (oldest first). An unparseable or
/// unrecognised line, a `tool_end` whose `id` never had a matching
/// `tool_start` in this same window (the start fell outside the last
/// 64 KiB), and a `tool_start` with no `id` at all, are all skipped
/// rather than treated as an error -- the same tolerance the tailer
/// itself has for a cut-off last line or a replaced file.
pub fn build_transcript_view(lines: &[Value], truncated: bool) -> TranscriptView {
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut by_id: BTreeMap<u64, usize> = BTreeMap::new();
    let mut max_id: Option<u64> = None;
    let mut last_reply = None;
    for line in lines {
        let Some(ty) = line.get("type").and_then(Value::as_str) else {
            continue;
        };
        let t = line.get("t").and_then(Value::as_str).map(str::to_string);
        match ty {
            "tool_start" => {
                let Some(id) = line.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                // Issue #637 review: a retried attempt's own ids start
                // over at 1, so an id no greater than the highest
                // seen so far means the previous attempt is gone --
                // whatever it left unmatched is never getting a
                // `tool_end` and must stop reading as running.
                if max_id.is_some_and(|max| id <= max) {
                    for idx in by_id.values() {
                        if calls[*idx].ok.is_none() {
                            calls[*idx].stopped = true;
                        }
                    }
                    by_id.clear();
                }
                max_id = Some(max_id.map_or(id, |max| max.max(id)));
                let tool = line
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let target = line
                    .get("target")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                calls.push(ToolCall {
                    start_t: t,
                    end_t: None,
                    tool,
                    target,
                    ok: None,
                    ms: None,
                    lines_added: None,
                    lines_removed: None,
                    stopped: false,
                });
                by_id.insert(id, calls.len() - 1);
            }
            "tool_end" => {
                let Some(id) = line.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(&idx) = by_id.get(&id) else {
                    continue;
                };
                let call = &mut calls[idx];
                call.end_t = t;
                call.ok = line.get("ok").and_then(Value::as_bool);
                call.ms = line.get("ms").and_then(Value::as_u64);
                call.lines_added = line.get("lines_added").and_then(Value::as_u64);
                call.lines_removed = line.get("lines_removed").and_then(Value::as_u64);
            }
            "reply" => {
                last_reply = line.get("text").and_then(Value::as_str).map(str::to_string);
            }
            _ => {}
        }
    }
    let tests = calls
        .iter()
        .rev()
        .find(|c| !c.stopped && c.target.as_deref().is_some_and(is_test_command))
        .map(|c| match c.ok {
            None => TestOutcome::Running,
            Some(true) => TestOutcome::Ok,
            Some(false) => TestOutcome::Failed,
        });
    TranscriptView {
        calls,
        tests,
        last_reply,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(v: Value) -> Value {
        v
    }

    #[test]
    fn unmatched_tool_start_shows_as_running() {
        let lines = vec![line(
            json!({"t": "2024-01-01T00:00:00Z", "type": "tool_start", "id": 1, "tool": "bash", "target": "pytest -k x"}),
        )];
        let view = build_transcript_view(&lines, false);
        assert_eq!(view.calls.len(), 1);
        assert!(view.calls[0].is_running());
        assert_eq!(view.tests, Some(TestOutcome::Running));
    }

    #[test]
    fn matched_tool_end_resolves_ok_and_tests() {
        let lines = vec![
            json!({"t": "t0", "type": "tool_start", "id": 1, "tool": "bash", "target": "pytest -k x"}),
            json!({"t": "t1", "type": "tool_end", "id": 1, "ok": true, "ms": 41000}),
        ];
        let view = build_transcript_view(&lines, false);
        assert!(!view.calls[0].is_running());
        assert_eq!(view.tests, Some(TestOutcome::Ok));
    }

    #[test]
    fn failed_test_command_reads_failed() {
        let lines = vec![
            json!({"t": "t0", "type": "tool_start", "id": 1, "tool": "bash", "target": "pytest -k timeout"}),
            json!({"t": "t1", "type": "tool_end", "id": 1, "ok": false, "ms": 500}),
        ];
        let view = build_transcript_view(&lines, false);
        assert_eq!(view.tests, Some(TestOutcome::Failed));
    }

    #[test]
    fn last_test_command_wins_over_an_earlier_one() {
        let lines = vec![
            json!({"t": "t0", "type": "tool_start", "id": 1, "tool": "bash", "target": "pytest -k timeout"}),
            json!({"t": "t1", "type": "tool_end", "id": 1, "ok": false, "ms": 500}),
            json!({"t": "t2", "type": "tool_start", "id": 2, "tool": "bash", "target": "pytest -k timeout"}),
            json!({"t": "t3", "type": "tool_end", "id": 2, "ok": true, "ms": 500}),
        ];
        let view = build_transcript_view(&lines, false);
        assert_eq!(view.tests, Some(TestOutcome::Ok));
    }

    #[test]
    fn no_test_command_at_all_gives_no_tests_field() {
        let lines = vec![
            json!({"t": "t0", "type": "tool_start", "id": 1, "tool": "read", "target": "PLAN.md"}),
            json!({"t": "t1", "type": "tool_end", "id": 1, "ok": true, "ms": 5}),
        ];
        let view = build_transcript_view(&lines, false);
        assert_eq!(view.tests, None);
    }

    #[test]
    fn a_tool_end_with_no_matching_start_in_this_window_is_skipped() {
        let lines = vec![json!({"t": "t1", "type": "tool_end", "id": 99, "ok": true, "ms": 5})];
        let view = build_transcript_view(&lines, false);
        assert!(view.calls.is_empty());
    }

    #[test]
    fn an_unparseable_or_unknown_line_is_skipped_not_an_error() {
        let lines = vec![
            json!({"type": "something_else"}),
            json!({"no_type_field": true}),
        ];
        let view = build_transcript_view(&lines, false);
        assert!(view.calls.is_empty());
        assert_eq!(view.tests, None);
    }

    #[test]
    fn a_retried_attempts_lower_id_closes_out_the_earlier_unmatched_call() {
        let lines = vec![
            json!({"t": "t0", "type": "tool_start", "id": 1, "tool": "bash", "target": "pytest -k x"}),
            json!({"t": "t1", "type": "tool_start", "id": 1, "tool": "read", "target": "PLAN.md"}),
            json!({"t": "t2", "type": "tool_end", "id": 1, "ok": true, "ms": 5}),
        ];
        let view = build_transcript_view(&lines, false);
        assert_eq!(view.calls.len(), 2);
        assert!(view.calls[0].stopped);
        assert!(!view.calls[0].is_running());
        assert!(!view.calls[1].stopped);
        assert_eq!(view.calls[1].ok, Some(true));
        // The stopped call was itself a test command, but it never
        // got its own outcome -- it must not be read as "still
        // running" forever, or as "failed", either.
        assert_eq!(view.tests, None);
    }

    #[test]
    fn the_last_reply_text_is_kept() {
        let lines = vec![
            json!({"t": "t0", "type": "reply", "text": "first"}),
            json!({"t": "t1", "type": "reply", "text": "second"}),
        ];
        let view = build_transcript_view(&lines, false);
        assert_eq!(view.last_reply, Some("second".to_string()));
    }

    #[test]
    fn tool_glyph_reads_both_engines_own_casing() {
        assert_eq!(tool_glyph("Edit"), '▒');
        assert_eq!(tool_glyph("write"), '▒');
        assert_eq!(tool_glyph("Bash"), '█');
        assert_eq!(tool_glyph("shell_exec"), '█');
        assert_eq!(tool_glyph("Read"), '░');
        assert_eq!(tool_glyph("Grep"), '░');
        assert_eq!(tool_glyph("mcp__custom__frobnicate"), '·');
    }

    #[test]
    fn tool_kind_classify_matches_tool_glyph() {
        assert_eq!(ToolKind::classify("Edit"), ToolKind::Edit);
        assert_eq!(ToolKind::classify("Bash"), ToolKind::Bash);
        assert_eq!(ToolKind::classify("Read"), ToolKind::Read);
        assert_eq!(ToolKind::classify("frobnicate"), ToolKind::Other);
    }
}
