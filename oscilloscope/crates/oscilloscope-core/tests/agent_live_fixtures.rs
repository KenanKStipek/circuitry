//! Issue #637: recorded fixtures from a real `cof` run against the
//! reference fake `pi`/`claude` CLIs (`scripts/record_fixtures.py`'s
//! `AGENT_LIVE_CASES`, `tests/fixtures/agent_live_pi`,
//! `tests/fixtures/agent_live_claude_code` and
//! `tests/fixtures/agent_live_pi_cancelled`), loaded from their own
//! `snapshots.jsonl`/transcript files -- never hand-copied JSON -- so
//! `AgentLive`'s own parsing is checked against what the real agent
//! tool actually writes, not a shape this crate invented on its own.

use std::path::{Path, PathBuf};

use oscilloscope_core::model::AgentLive;
use oscilloscope_core::observe::{TRANSCRIPT_TAIL_BYTES, tail_transcript_lines};
use oscilloscope_core::transcript::{TestOutcome, build_transcript_view};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn load_snapshots(name: &str) -> Vec<Value> {
    let dir = fixtures_dir().join(name);
    let text = std::fs::read_to_string(dir.join("snapshots.jsonl"))
        .unwrap_or_else(|e| panic!("{name}: couldn't read snapshots.jsonl: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let wrapper: Value = serde_json::from_str(line).expect("valid JSON line");
            wrapper["state"].clone()
        })
        .collect()
}

/// `None` for any snapshot that doesn't have this exact path yet
/// (the first live-state write has no `prime` at all, DESIGN.md
/// §1.1, and most snapshots never reach a tree-loop pass's own
/// nested path) -- a caller walking every snapshot skips these
/// rather than treating "not there yet" as a test failure.
fn agent_meta<'a>(state: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut node = state.get("prime")?;
    for segment in path {
        node = node.get(segment)?;
    }
    node.get("meta")
}

/// The marker strings the reference fakes embed in prompt/tool-
/// argument/tool-output/reply/thinking text (`tests/agent_live_fakes/
/// README.md`) -- none of them may ever reach `meta.live`, `meta.raw`
/// or `--events` (`docs/plugins/agent.md`'s own "Never in meta.live,
/// meta.raw or --events" list). `MARKER_REPLY` is the one marker that
/// legitimately appears elsewhere (`value`, the agent's own final
/// reply text by the tool's documented contract), so this only ever
/// inspects `meta` and `--events`, never `value`.
const FORBIDDEN_IN_META: &[&str] = &[
    "MARKER_PROMPT",
    "MARKER_THINKING",
    "MARKER_TOOL_OUTPUT",
    "MARKER_FILE_CONTENT",
    "MARKER_REPLY",
    "MARKER_COMMAND",
    "MARKER_PATH",
];

fn assert_no_marker_in_meta(name: &str) {
    for state in load_snapshots(name) {
        let Some(prime) = state.get("prime") else {
            continue;
        };
        let text = collect_meta_text(prime);
        for marker in FORBIDDEN_IN_META {
            assert!(
                !text.contains(marker),
                "{name}: {marker} leaked into some node's own meta"
            );
        }
    }
    let events_path = fixtures_dir().join(name).join("events.jsonl");
    if let Ok(events_text) = std::fs::read_to_string(&events_path) {
        for marker in FORBIDDEN_IN_META {
            assert!(
                !events_text.contains(marker),
                "{name}: {marker} leaked into --events"
            );
        }
    }
}

/// Every `meta` object's own text anywhere under `node`, depth-first
/// -- `value` is deliberately excluded (the one field the agent tool
/// documents as carrying the reply text itself).
fn collect_meta_text(node: &Value) -> String {
    let mut out = String::new();
    collect_meta_text_into(node, &mut out);
    out
}

fn collect_meta_text_into(node: &Value, out: &mut String) {
    let Some(obj) = node.as_object() else {
        return;
    };
    if let Some(meta) = obj.get("meta") {
        out.push_str(&meta.to_string());
    }
    for (key, child) in obj {
        if key == "value" || key == "meta" {
            continue;
        }
        collect_meta_text_into(child, out);
    }
}

#[test]
fn agent_live_pi_fixture_never_leaks_prompt_reply_or_tool_content_into_meta_or_events() {
    assert_no_marker_in_meta("agent_live_pi");
}

#[test]
fn agent_live_claude_code_fixture_never_leaks_prompt_reply_or_tool_content_into_meta_or_events() {
    assert_no_marker_in_meta("agent_live_claude_code");
}

/// The first snapshot whose own `meta.live.turns` has reached
/// *turns*, for the given dotted-from-`prime` path -- timing jitter
/// make a snapshot's own elapsed `t` useless as a selector, but the
/// reference fake's own token/tool numbers are deterministic at each
/// turn regardless of how long the real sleep between them took.
fn first_live_at_turn(name: &str, path: &[&str], turns: u64) -> AgentLive {
    for state in load_snapshots(name) {
        let Some(meta) = agent_meta(&state, path) else {
            continue;
        };
        if let Some(live) = meta.get("live").and_then(AgentLive::from_value) {
            if live.turns == Some(turns) {
                return live;
            }
        }
    }
    panic!("{name}: no live snapshot at prime.{path:?} ever reached turns={turns}");
}

/// `fake_pi.py`'s own first turn: a `bash` tool call, with the usage
/// its own `message_end` reports -- matches the real numbers recorded
/// mid-run (DESIGN.md: never hand-copied, read straight from the
/// fixture here instead).
#[test]
fn agent_live_pi_fixture_shows_the_first_turns_real_usage() {
    let live = first_live_at_turn("agent_live_pi", &["top_agent"], 1);
    assert_eq!(live.engine, Some("pi".to_string()));
    assert_eq!(live.tool_calls, Some(1));
    assert_eq!(live.tokens_sent, Some(2100));
    assert_eq!(live.tokens_received, Some(40));
    assert_eq!(live.cost_usd, Some(0.012));
    assert_eq!(live.tokens_cached_in, Some(800));
    assert_eq!(
        live.current_tool.as_ref().map(|t| t.name.as_str()),
        Some("bash")
    );
}

/// The finished effect's own `meta.live` is gone (removed once the
/// attempt ends, `docs/plugins/agent.md`), and the generic top-level
/// `tokens_sent`/`tokens_received`/`cost_usd` mirror carry the same
/// totals `meta.raw` does.
#[test]
fn agent_live_pi_fixtures_final_state_has_no_live_left() {
    let states = load_snapshots("agent_live_pi");
    let last = states.last().expect("at least one snapshot");
    let meta = agent_meta(last, &["top_agent"]).expect("top_agent must exist in the final state");
    assert!(meta.get("live").is_none_or(Value::is_null));
    assert_eq!(meta["tokens_sent"], 11550);
    assert_eq!(meta["tokens_received"], 130);
    assert_eq!(meta["cost_usd"], 0.04);
    assert_eq!(meta["raw"]["engine"], "pi");
    assert_eq!(meta["raw"]["turns"], 3);
}

/// A tree-loop pass's own agent (DESIGN.md, DESIGN.md §1.4: a running
/// child's own effect shows in state the same way a top-level one
/// does) gets a real `meta.live` of its own too.
#[test]
fn agent_live_pi_fixtures_tree_loop_pass_also_gets_a_live_summary() {
    let live = first_live_at_turn("agent_live_pi", &["fan", "iter_0", "tree_agent"], 1);
    assert_eq!(live.engine, Some("pi".to_string()));
    assert_eq!(live.tool_calls, Some(1));
}

/// The real transcript `cof` wrote for `top_agent`, tailed and parsed
/// the same way osi's own TUI does: the last test command
/// (`pytest -k MARKER_COMMAND`) resolves `ok`, and both tool calls
/// (`bash`, `edit`) appear in order.
#[test]
fn agent_live_pi_fixtures_transcript_resolves_tests_ok_and_both_tool_calls() {
    let path = fixtures_dir()
        .join("agent_live_pi")
        .join("top_agent_transcript.jsonl");
    let lines = tail_transcript_lines(&path, TRANSCRIPT_TAIL_BYTES);
    assert!(
        !lines.is_empty(),
        "the recorded transcript must not be empty"
    );
    let view = build_transcript_view(&lines, false);
    assert_eq!(view.tests, Some(TestOutcome::Ok));
    assert_eq!(view.calls.len(), 2);
    assert_eq!(view.calls[0].tool, "bash");
    assert_eq!(view.calls[1].tool, "edit");
    assert_eq!(view.calls[1].lines_added, Some(3));
    assert_eq!(view.calls[1].lines_removed, Some(1));
}

/// `fake_claude.py`'s own subagent (`Task` → a nested `Read` call):
/// its own 9000-token usage must never count toward the parent
/// session's `tokens_sent`, while its tool call still counts --
/// exactly `docs/plugins/agent.md`'s own rule, checked against the
/// real recorded numbers rather than a hand-written assertion of
/// what they "should" be.
#[test]
fn agent_live_claude_code_fixture_excludes_the_subagents_own_tokens() {
    let live = first_live_at_turn("agent_live_claude_code", &["top_agent"], 1);
    assert_eq!(
        live.tool_calls,
        Some(2),
        "the Task call and its subagent's Read"
    );
    assert_eq!(
        live.tokens_sent,
        Some(1600),
        "only the parent call's own usage, never the subagent's 9000"
    );
    // Claude Code never reports these two live (`docs/plugins/agent.md`).
    assert_eq!(live.tokens_received, None);
    assert_eq!(live.cost_usd, None);
}

/// Issue #637's acceptance item 2: `scripts/record_fixtures.py`'s
/// `agent_live_pi_cancelled` case sends SIGINT mid the first tool
/// call, the same shape `cancel_state.py` confirmed real `cof`
/// leaves behind.
#[test]
fn agent_live_pi_cancelled_fixture_never_leaks_prompt_reply_or_tool_content_into_meta_or_events() {
    assert_no_marker_in_meta("agent_live_pi_cancelled");
}

/// The cancelled run's own final state: `prime` itself has ended
/// (SIGINT stops the whole run), but `top_agent`'s own `meta.live`
/// is still there, with no `completed_at` of its own --
/// `docs/plugins/agent.md`'s "a watcher shows it as stopped" case,
/// the fixture a front end's own stopped-agent rendering reads.
#[test]
fn agent_live_pi_cancelled_fixtures_final_state_keeps_live_with_no_completed_at() {
    let states = load_snapshots("agent_live_pi_cancelled");
    let last = states.last().expect("at least one snapshot");
    assert!(
        last["prime"]["meta"]["completed_at"].is_string(),
        "the run itself must have ended"
    );
    let meta = agent_meta(last, &["top_agent"]).expect("top_agent must exist in the final state");
    assert!(
        meta["completed_at"].is_null(),
        "the agent's own attempt never completed"
    );
    let live = meta
        .get("live")
        .and_then(AgentLive::from_value)
        .expect("meta.live must still be there");
    assert_eq!(live.engine, Some("pi".to_string()));
    assert_eq!(live.turns, Some(1));
    assert_eq!(live.tokens_sent, Some(2100));
}
