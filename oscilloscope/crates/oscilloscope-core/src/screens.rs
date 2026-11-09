//! Screens (issue #626): `Screen` names the five screens `osi`
//! switches between with `0`-`3`; [`header_line`] builds the one
//! line every screen shares (replacing the old three-row header);
//! [`TapeScreen`] is screen 0's own model (issue #678, replacing the
//! old Overview), built from exactly the same inputs `render::build`
//! already has (the live state, `--events`, the plan and
//! `runtime.last_run.totals` -- no engine change). Neither function
//! draws anything -- no terminal code lives here (DESIGN.md §6.1),
//! so `oscilloscope`'s `tui.rs` is the only thing that turns a
//! segment's role or a row's field into colour and position.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::format::{ClockZone, format_clock_time};
use crate::model::{NodeMeta, RowStatus, RunModel, RunStatus, StatusKind};
use crate::observe::duration_seconds;
use crate::plan::{Flow, LeafEffectKind, PlanEntryKind, PlanTree};
use crate::render::{Header, Row, RowKind, TokenBlock, details_for};

/// Which of the four screens is on display (issue #650: all four
/// now have a screen to show).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    /// The home screen (issue #678's design v1, replacing the old
    /// overview): one time axis, the Now band, a fixed-scale token
    /// column.
    #[default]
    Tape,
    Signal,
    Flow,
    Cost,
}

impl Screen {
    pub fn number(self) -> u8 {
        match self {
            Screen::Tape => 0,
            Screen::Signal => 1,
            Screen::Flow => 2,
            Screen::Cost => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Screen::Tape => "tape",
            Screen::Signal => "signal",
            Screen::Flow => "flow",
            Screen::Cost => "cost",
        }
    }

    pub fn is_available(self) -> bool {
        matches!(
            self,
            Screen::Tape | Screen::Signal | Screen::Flow | Screen::Cost
        )
    }

    /// The screen a `0`-`3` key names, or `None` for any other key,
    /// or for a digit whose screen isn't built yet (`1`) --
    /// callers never need a second check of [`Screen::is_available`].
    pub fn from_key(c: char) -> Option<Screen> {
        let screen = match c {
            '0' => Screen::Tape,
            '1' => Screen::Signal,
            '2' => Screen::Flow,
            '3' => Screen::Cost,
            _ => return None,
        };
        screen.is_available().then_some(screen)
    }

    /// Every screen with a tab, in tab order (issue #626's header).
    pub fn available() -> &'static [Screen] {
        &[Screen::Tape, Screen::Signal, Screen::Flow, Screen::Cost]
    }
}

// ---------------------------------------------------------------------
// The header line
// ---------------------------------------------------------------------

/// What a [`HeaderSegment`]'s text means, for a front end to colour
/// (never computed here -- DESIGN.md §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderRole {
    /// The scope icon's own trace (`oscilloscope_core::icon::
    /// header_trace_text`) -- coloured by run status, same as the
    /// old three-row icon was.
    Icon,
    Document,
    /// The `♥` shown while the engine process is alive.
    Alive,
    Status,
    Elapsed,
    Clock,
    Eta,
    Report,
    Warning,
    TabActive,
    TabInactive,
    /// Plain separators (` · `, the gap before the tabs) and
    /// anything else with no colour of its own.
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderSegment {
    pub role: HeaderRole,
    pub text: String,
}

fn seg(role: HeaderRole, text: impl Into<String>) -> HeaderSegment {
    HeaderSegment {
        role,
        text: text.into(),
    }
}

/// The plain concatenation of every segment's own text -- what
/// `--glance` prints, and what every truncation test here compares
/// against.
pub fn header_line_text(segments: &[HeaderSegment]) -> String {
    segments.iter().map(|s| s.text.as_str()).collect()
}

fn segments_width(segments: &[HeaderSegment]) -> usize {
    segments.iter().map(|s| s.text.chars().count()).sum()
}

/// Builds the one-line header every screen shares (issue #626),
/// replacing the old three-row header -- the only source for both
/// the TUI header and `--glance`'s own line, so the two can never
/// disagree about what it says. `now`/`zone` format the clock time
/// and the ETA; `icon_frame`/`animate` drive the scope icon's own
/// trace, exactly as the old icon widget did.
///
/// Truncation order, when `width` is too narrow for the full line
/// (issue #626): the tabs shorten to their own numbers first; then
/// the report truncates, down to a bare `▸`; then the document
/// truncates, keeping its own tail; then the ETA is dropped
/// entirely. Status, elapsed and the clock never truncate.
#[allow(clippy::too_many_arguments)]
pub fn header_line(
    header: &crate::render::Header,
    now: &str,
    zone: &dyn ClockZone,
    width: u16,
    animate: bool,
    icon_frame: u64,
    current: Option<Screen>,
) -> Vec<HeaderSegment> {
    let running = matches!(header.status, RunStatus::Running);
    let icon_text = crate::icon::header_trace_text(icon_frame, running, animate);

    let (status_text, status_role) = match header.status {
        RunStatus::Running => ("running", HeaderRole::Status),
        RunStatus::Ok => ("ok", HeaderRole::Status),
        RunStatus::Failed => ("failed", HeaderRole::Status),
        RunStatus::Cancelled => ("cancelled", HeaderRole::Status),
        RunStatus::Aborted => ("aborted", HeaderRole::Status),
    };
    let elapsed_text = crate::format::format_duration(header.elapsed_s);
    let clock_text = format_clock_time(now, zone).unwrap_or_default();
    // Issue #649: the whole-run ETA from history, once there are
    // enough runs, in place of the innermost loop's own -- `ETA ?`
    // once the run is past its own usual end, rather than a time
    // that keeps sliding.
    let eta_text = if header.history_eta_past_usual {
        Some("ETA ?".to_string())
    } else if let Some(hhmmss) = header
        .history_eta_ts
        .as_deref()
        .and_then(|ts| format_clock_time(ts, zone))
    {
        Some(format!("ETA ~{}", &hhmmss[..5.min(hhmmss.len())]))
    } else {
        header
            .innermost_loop_eta_ts
            .as_deref()
            .and_then(|ts| format_clock_time(ts, zone))
            .map(|hhmmss| format!("loop ETA ~{}", &hhmmss[..5.min(hhmmss.len())]))
    };
    let report_full = header.latest_report.as_ref().map(|report| {
        let prefix = match report.level {
            crate::observe::ReportLevel::Warning => "warning: ",
            crate::observe::ReportLevel::Info => "",
        };
        let message = report.message.split('\n').next().unwrap_or("");
        (
            format!("▸ {prefix}{message}"),
            matches!(report.level, crate::observe::ReportLevel::Warning),
        )
    });

    // Full labels first; `shrink_tabs` below replaces this with the
    // bare-number form when the line is still too wide.
    let mut tabs: Vec<(Screen, String)> = Screen::available()
        .iter()
        .map(|&screen| (screen, format!("{} {}", screen.number(), screen.label())))
        .collect();

    let build =
        |document: &str, report: Option<&str>, eta: Option<&str>, tabs: &[(Screen, String)]| {
            let mut out = vec![seg(HeaderRole::Icon, icon_text.clone())];
            out.push(seg(HeaderRole::Plain, " "));
            out.push(seg(HeaderRole::Document, document.to_string()));
            out.push(seg(HeaderRole::Plain, " \u{b7} "));
            if header.engine_alive {
                out.push(seg(HeaderRole::Alive, "\u{2665}"));
                out.push(seg(HeaderRole::Plain, " "));
            }
            out.push(seg(status_role, status_text));
            out.push(seg(HeaderRole::Plain, " "));
            out.push(seg(HeaderRole::Elapsed, elapsed_text.clone()));
            out.push(seg(HeaderRole::Plain, " \u{b7} "));
            out.push(seg(HeaderRole::Clock, clock_text.clone()));
            if let Some(eta) = eta {
                out.push(seg(HeaderRole::Plain, " \u{b7} "));
                out.push(seg(HeaderRole::Eta, eta.to_string()));
            }
            if let Some(report_text) = report {
                let (_, warning) = report_full
                    .as_ref()
                    .expect("report text implies report_full");
                out.push(seg(HeaderRole::Plain, " \u{b7} "));
                out.push(seg(
                    if *warning {
                        HeaderRole::Warning
                    } else {
                        HeaderRole::Report
                    },
                    report_text.to_string(),
                ));
            }
            // `--glance` (issue #626) shows the header line with no tabs
            // at all -- `current: None`.
            if let Some(current) = current {
                out.push(seg(HeaderRole::Plain, "   "));
                for (i, (screen, label)) in tabs.iter().enumerate() {
                    if i > 0 {
                        out.push(seg(HeaderRole::Plain, "  "));
                    }
                    let active = *screen == current;
                    let text = if active {
                        format!("[{label}]")
                    } else {
                        label.clone()
                    };
                    out.push(seg(
                        if active {
                            HeaderRole::TabActive
                        } else {
                            HeaderRole::TabInactive
                        },
                        text,
                    ));
                }
            }
            out
        };

    let report_text = report_full.as_ref().map(|(text, _)| text.clone());
    let original_document = header.document.clone();
    let mut document = original_document.clone();
    let mut report = report_text.clone();
    let mut eta = eta_text.clone();
    let target = width as usize;

    let fits =
        |document: &str, report: Option<&str>, eta: Option<&str>, tabs: &[(Screen, String)]| {
            segments_width(&build(document, report, eta, tabs)) <= target
        };

    if fits(&document, report.as_deref(), eta.as_deref(), &tabs) {
        return build(&document, report.as_deref(), eta.as_deref(), &tabs);
    }

    // 1. Tabs shrink to their own numbers.
    tabs = Screen::available()
        .iter()
        .map(|&screen| (screen, screen.number().to_string()))
        .collect();
    if fits(&document, report.as_deref(), eta.as_deref(), &tabs) {
        return build(&document, report.as_deref(), eta.as_deref(), &tabs);
    }

    // 2. The report truncates, down to a bare `▸` -- solved
    // arithmetically (the exact budget left for it once everything
    // else is at its current width), not by rebuilding the whole
    // line once per character shaved off: that rebuilt every
    // segment's text on every step, costing O(report length squared)
    // on a single long report line.
    if let Some(full) = &report_text {
        let full_len = full.chars().count();
        let width_without_report = segments_width(&build(&document, None, eta.as_deref(), &tabs));
        // The 3-column ` · ` separator only actually appears once
        // there's a report at all.
        let available = target.saturating_sub(width_without_report + 3);
        report = Some(if available <= 1 {
            "▸".to_string()
        } else {
            truncate_tail(full, available.min(full_len))
        });
        if fits(&document, report.as_deref(), eta.as_deref(), &tabs) {
            return build(&document, report.as_deref(), eta.as_deref(), &tabs);
        }
    }

    // 3. The document truncates, keeping its own tail -- the same
    // arithmetic approach as the report's own, above.
    {
        let empty = String::new();
        let width_without_document =
            segments_width(&build(&empty, report.as_deref(), eta.as_deref(), &tabs));
        let available = target.saturating_sub(width_without_document);
        document = shrink_document(&original_document, available);
        if fits(&document, report.as_deref(), eta.as_deref(), &tabs) {
            return build(&document, report.as_deref(), eta.as_deref(), &tabs);
        }
    }

    // 4. The ETA is dropped entirely -- freeing up room the document
    // just gave up for nothing, so step 3's own truncation (against
    // the ETA still being there) is redone from the original document
    // now that it's gone, rather than left however small step 3 left
    // it. Status, elapsed and the clock never truncate -- if the line
    // is still too wide after this, it simply overflows (the
    // renderer clips it, same as any other line that doesn't fit).
    eta = None;
    let empty = String::new();
    let width_without_document = segments_width(&build(&empty, report.as_deref(), None, &tabs));
    let available = target.saturating_sub(width_without_document);
    document = shrink_document(&original_document, available);
    build(&document, report.as_deref(), eta.as_deref(), &tabs)
}

/// [`truncate_head_keep_tail`]'s own `available`-is-the-whole-budget
/// wrapper, shared by the document-truncation steps above: the full
/// document when it already fits in `available` chars, truncated to
/// exactly `available` otherwise (0 drops it entirely).
fn shrink_document(original: &str, available: usize) -> String {
    let len = original.chars().count();
    if available >= len {
        return original.to_string();
    }
    if available == 0 {
        return String::new();
    }
    truncate_head_keep_tail(original, available)
}

/// [`truncate_head_keep_tail`]'s own counterpart: cuts the *end* and
/// marks it with a trailing `…` -- the report's own text has its
/// meaning at the front, unlike a path.
fn truncate_tail(label: &str, max: usize) -> String {
    let count = label.chars().count();
    if count <= max {
        return label.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!(
        "{}…",
        label
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
    )
}

/// Keeps the document's own tail (where the file name is) and marks
/// the cut with a leading `…`.
fn truncate_head_keep_tail(label: &str, max: usize) -> String {
    let count = label.chars().count();
    if count <= max {
        return label.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let skip = count - max.saturating_sub(1);
    format!("…{}", label.chars().skip(skip).collect::<String>())
}

// ---------------------------------------------------------------------
// The overview (screen 0)
// ---------------------------------------------------------------------

/// A row in the phase strip -- the document's own top-level steps,
/// already breadcrumb-folded (`label` comes straight from `Row`,
/// which `fold::fold_rows` already folded a single-child wrapper
/// chain into one row for, e.g. `"review \u203a fix"`).
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseStep {
    pub path: String,
    pub label: String,
    pub status: RowStatus,
    pub start_ts: Option<String>,
    pub duration_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FocusDetail {
    Prompt {
        /// `sent`'s own token count (actual once the prompt has gone
        /// out, `estimated` while it's still in flight) -- `None`
        /// only while even the estimate has nothing to count from.
        tokens: Option<u64>,
        estimated: bool,
        snippet: Option<String>,
    },
    Tool {
        command: Option<String>,
    },
    /// A `tool` step whose own `provider` is `"agent"` (DESIGN.md's
    /// token-bar rule). `live` carries `meta.live` (issue #637) for as
    /// long as the agent tool is tracking the session -- running, or
    /// an interrupted run's own stale leftover (`AgentLive::is_recent`
    /// tells the two apart); it is `None` once the step has finished
    /// normally, at which point the generic [`FocusedStep::tokens`],
    /// [`FocusedStep::cost_usd`] and [`FocusedStep::value`] already
    /// carry its totals the same way any other tool step's do, and
    /// `note` is the one thing left to say (only ever shown while the
    /// step is still running, in the brief window before the very
    /// first `meta.live` write).
    Agent {
        /// The first line of `params_rendered.prompt`, or the file's
        /// own path for a prompt read from one (`docs/plugins/
        /// agent.md`'s "A prompt from a file") -- never `params_
        /// rendered.command`, which an agent effect's own params
        /// never set at all.
        asked: Option<String>,
        live: Option<Box<crate::model::AgentLive>>,
        /// `live.is_some()` and its own `updated_at` is no longer
        /// within 5 seconds of `now` (`AgentLive::is_recent`) -- an
        /// interrupted run's own leftover `meta.live`, never removed
        /// because nothing was left running to remove it
        /// (`docs/plugins/agent.md`: "a watcher shows it as
        /// stopped"). Computed once here, from the same `now` this
        /// step was built with, so a front end never needs its own
        /// wall clock to tell a stale summary from a live one.
        stale: bool,
        /// How long `live.current_tool` has been running, from its
        /// own `started_at` to `now` (issue #647) -- `None` while
        /// `stale` (nothing is actually running any more to time) or
        /// with no `current_tool`/unparsable `started_at` at all.
        current_tool_elapsed_s: Option<f64>,
        note: &'static str,
        /// `meta.raw.transcript`, set once the session has finished
        /// (`meta.raw` only exists then) -- `None` while running, same
        /// as [`Self::Agent::live`]'s own transcript path reads it
        /// from `meta.live` instead for as long as that lasts.
        raw_transcript: Option<String>,
        /// `meta.raw.files_changed`.
        raw_files_changed: Option<u64>,
        /// `meta.raw.turns`.
        raw_turns: Option<u64>,
        /// `meta.raw.tool_calls` -- the Flow screen's selected pane
        /// (issue #650) needs this once the session has finished,
        /// the same way it reads `meta.live.tool_calls` while running.
        raw_tool_calls: Option<u64>,
        /// `meta.raw.tool_counts`, the same `{calls, ms}`-per-name
        /// shape as `meta.live.tools` -- empty once finished with no
        /// tool calls at all, same as a running session with none.
        raw_tool_counts: BTreeMap<String, crate::model::AgentLiveToolUsage>,
    },
    Plain,
}

/// [`FocusDetail::Agent::asked`]: the first line of `params_rendered.
/// prompt` (a plain string), or the file's own path when the prompt
/// was read from one (the `{file, within}` form `docs/plugins/
/// agent.md`'s "A prompt from a file" records, resolved paths
/// included) -- `None` for neither shape, or no `prompt` at all.
fn agent_asked_text(meta: &Value) -> Option<String> {
    let prompt = meta.pointer("/params_rendered/prompt")?;
    if let Some(text) = prompt.as_str() {
        return Some(text.lines().next().unwrap_or(text).to_string());
    }
    prompt
        .get("file")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// [`FocusDetail::Agent`]'s own `raw_transcript`/`raw_files_changed`/
/// `raw_turns`, all read from `meta.raw` -- present only once the
/// session has actually finished and reported one.
#[derive(Default)]
struct AgentRawSummary {
    transcript: Option<String>,
    files_changed: Option<u64>,
    turns: Option<u64>,
    tool_calls: Option<u64>,
    tool_counts: BTreeMap<String, crate::model::AgentLiveToolUsage>,
}

fn agent_raw_summary(meta: &Value) -> AgentRawSummary {
    AgentRawSummary {
        transcript: meta
            .pointer("/raw/transcript")
            .and_then(Value::as_str)
            .map(str::to_string),
        files_changed: meta.pointer("/raw/files_changed").and_then(Value::as_u64),
        turns: meta.pointer("/raw/turns").and_then(Value::as_u64),
        tool_calls: meta.pointer("/raw/tool_calls").and_then(Value::as_u64),
        tool_counts: meta
            .pointer("/raw/tool_counts")
            .map(crate::model::AgentLiveToolUsage::map_from_value)
            .unwrap_or_default(),
    }
}

/// [`FocusDetail::Agent`]'s own fallback note: shown only when the
/// step has neither live data nor a finished result yet -- the brief
/// window between the tool starting and the agent tool's first
/// `meta.live` write.
pub const AGENT_LIVE_DATA_NOTE: &str = "live session data isn't available yet";

#[derive(Debug, Clone, PartialEq)]
pub struct FocusedStep {
    pub path: String,
    pub breadcrumb: String,
    pub kind_label: &'static str,
    pub provider_or_model: Option<String>,
    pub since_ts: Option<String>,
    pub elapsed_s: Option<f64>,
    pub detail: FocusDetail,
    /// The step's own status (issue #635's selected pane: "produced"
    /// only shows once the step has actually finished, and a
    /// running one reads "(when done)" instead) -- the overview's
    /// own `NowFocus::Running`/`LastFinished` wrapper already carries
    /// this distinction for its one caller, but the Flow screen's
    /// selected pane shows whatever row is selected, running or not.
    pub status: RowStatus,
    /// Set once the step has failed (`render::Details::error`,
    /// never read from `meta` directly, so #587's fix for an
    /// unrecorded prompt flows through here unchanged).
    pub error: Option<String>,
    /// A tool step's own output value, truncated the same way
    /// `Details::value` already is.
    pub value: Option<String>,
    /// A prompt's own reply block (`Details::received`) -- `None`
    /// for a non-prompt step, or a prompt that hasn't replied yet.
    pub received: Option<TokenBlock>,
    /// `node_tokens`'s own sent-plus-received total, the same rule
    /// [`crate::flow::FlowRow::tokens`] uses.
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    /// A running step's own usual busy time from history (issue
    /// #649's now-pane bar) -- `None` below the 3-run threshold, or
    /// for a step that isn't actually running right now. Filled in
    /// by [`build_now_pane`]'s own `NowFocus::Running` branch, never
    /// by [`build_focused_step`] itself, which has no history of its
    /// own to read.
    pub usual_busy_p50_s: Option<f64>,
    /// [`Self::usual_busy_p50_s`]'s own p90 -- `None` below the
    /// 5-run threshold even with a p50 in hand.
    pub usual_busy_p90_s: Option<f64>,
}

fn node_for<'a>(row: &Row, flat: &'a BTreeMap<String, NodeMeta>) -> Option<&'a NodeMeta> {
    flat.get(&row.path)
}

/// Issue #635: carries `render::build_token_bar`'s own plan-
/// `provider` fallback over to the selected pane -- a tree-loop
/// pass's or `use` child's `agent` tool step has no state node (and
/// so no `meta.provider`) for as long as it runs, so without the
/// fallback it would never read as an agent here even though the
/// token bar already counts it as one.
fn is_agent_row(row: &Row, flat: &BTreeMap<String, NodeMeta>, plan: &PlanTree) -> bool {
    if row.kind != RowKind::Tool {
        return false;
    }
    let provider = node_for(row, flat)
        .and_then(|n| n.meta.get("provider"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            plan.match_path(&row.path)
                .and_then(|m| m.entries.first())
                .and_then(|e| e.provider.clone())
        });
    provider.is_some_and(|p| p.trim().eq_ignore_ascii_case("agent"))
}

/// Issue #637: once the whole run has ended, the first agent row
/// still holding `meta.live` with no `completed_at` of its own --
/// an interrupted session, never removed because nothing was left
/// running to remove it (`docs/plugins/agent.md`: "a watcher shows
/// it as stopped"). `None` once every agent either finished normally
/// or never started at all.
fn build_focused_step(
    row: &Row,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    now: &str,
    run_ended_at: Option<&str>,
) -> FocusedStep {
    let node = node_for(row, flat);
    // Issue #631: a step known only from `--events` (a tree-loop
    // pass's prompt, confirmed running before it ever gets a state
    // node of its own, DESIGN.md §1.4) has no `created_at` here to
    // read `since` from at all -- `RunModel::event_started_at` is the
    // same start time the token bar's own running-prompt count
    // already trusts for exactly this case.
    let since_ts = node
        .and_then(|n| n.created_at.clone())
        .or_else(|| model.event_started_at(&row.path).map(str::to_string));
    // A step that has already finished reads its own real duration
    // (`completed_at` - `created_at`), never `now - created_at`: once
    // the whole run has ended, `now` is wall-clock time at render,
    // long past the run's own end, which would otherwise inflate a
    // finished step's duration by however long it's been since. A
    // step that never got its own `completed_at` at all (an
    // interrupted session's leftover, issue #647) falls back to
    // `run_ended_at` instead of `now` when the caller has it (the
    // run's own end, not osi's wall clock) -- the session ended with
    // the run, so its own elapsed stops right there too.
    let elapsed_s =
        since_ts
            .as_deref()
            .and_then(|start| match node.and_then(|n| n.completed_at.as_deref()) {
                Some(end) => duration_seconds(start, end),
                None => duration_seconds(start, run_ended_at.unwrap_or(now)),
            });
    let details = details_for(&row.path, plan, model, state);
    let agent = is_agent_row(row, flat, plan);
    let detail = if agent {
        let live = node.and_then(|n| n.live.clone()).map(Box::new);
        let stale = live.as_deref().is_some_and(|l| !l.is_recent(now));
        // Issue #647: only while the session is actually running is
        // its current tool call actually running too -- once stale,
        // `current_tool` is whatever was last in flight when the
        // session was interrupted, not something still ticking.
        let current_tool_elapsed_s = (!stale)
            .then(|| live.as_deref().and_then(|l| l.current_tool.as_ref()))
            .flatten()
            .and_then(|t| duration_seconds(&t.started_at, now));
        let raw = node.map(|n| agent_raw_summary(&n.meta)).unwrap_or_default();
        FocusDetail::Agent {
            asked: node.and_then(|n| agent_asked_text(&n.meta)),
            live,
            stale,
            current_tool_elapsed_s,
            note: AGENT_LIVE_DATA_NOTE,
            raw_transcript: raw.transcript,
            raw_files_changed: raw.files_changed,
            raw_turns: raw.turns,
            raw_tool_calls: raw.tool_calls,
            raw_tool_counts: raw.tool_counts,
        }
    } else if row.kind == RowKind::Tool {
        FocusDetail::Tool {
            command: details
                .meta_summary
                .iter()
                .find(|(k, _)| k == "command")
                .map(|(_, v)| v.clone()),
        }
    } else if row.kind == RowKind::Prompt || row.kind == RowKind::Reflector {
        FocusDetail::Prompt {
            tokens: details.sent.as_ref().and_then(|b| b.tokens),
            estimated: details.sent.as_ref().is_some_and(|b| b.estimated),
            snippet: details.sent.as_ref().and_then(|b| b.snippet.clone()),
        }
    } else {
        FocusDetail::Plain
    };
    // Issue #631: same reasoning as `since_ts` above -- `row.dim` is
    // only ever populated from a state node's own `meta`, so a step
    // known only from events has nothing there yet either; the
    // compiled plan's own prompt op (`PlanTree::
    // prompt_model_or_provider`) is the one other source that can
    // name its model/adapter before state ever does.
    let provider_or_model = row
        .dim
        .clone()
        .or_else(|| plan.prompt_model_or_provider(&row.path));
    let tokens = node
        .map(|n| crate::model::node_tokens(&n.meta))
        .and_then(|(sent, received)| {
            let total = sent + received;
            (total > 0).then_some(total)
        });
    let cost_usd = node
        .and_then(|n| n.meta.get("cost_usd"))
        .and_then(Value::as_f64);
    FocusedStep {
        path: row.path.clone(),
        breadcrumb: row.label.clone(),
        kind_label: if agent { "agent" } else { row.kind.label() },
        provider_or_model,
        since_ts,
        elapsed_s,
        detail,
        status: row.status.clone(),
        error: details.error.clone(),
        value: details.value.clone(),
        received: details.received.clone(),
        tokens,
        cost_usd,
        usual_busy_p50_s: None,
        usual_busy_p90_s: None,
    }
}

fn build_phase_strip(rows: &[Row]) -> Vec<PhaseStep> {
    rows.iter()
        .filter(|r| r.depth == 1)
        .map(|r| PhaseStep {
            path: r.path.clone(),
            label: r.label.clone(),
            status: r.status.clone(),
            start_ts: None,
            duration_s: r.duration_s,
        })
        .collect()
}

// ---------------------------------------------------------------------
// The Tape (screen 0, issue #678)
// ---------------------------------------------------------------------

/// The Tape's own head rule (`design/tui-design.md` §2.1): which of
/// the four states the list's own top border reads.
#[derive(Debug, Clone, PartialEq)]
pub enum TapeHead {
    /// No events yet -- `header.run_start_ts` is `None`.
    Starting,
    /// A run under way, or an attached run already past its start.
    Normal,
    Ended {
        ok: bool,
        duration_s: f64,
        /// A handled failure's own row label and error, shown in the
        /// detail pane's own Outcome line.
        handled_failure: Option<(String, String)>,
    },
    Failed {
        error: String,
        /// The row that actually failed, so the detail pane can
        /// still show it even once the head rule reads Failed.
        failing_path: Option<String>,
    },
}

/// One row's own token column (§3.5/§3.6): carries the raw figures a
/// front end needs for *either* the fixed-scale bar or, after `t`,
/// the deviation view -- which one is drawn is a front end's own
/// choice, never baked in here, so toggling `t` never rebuilds the
/// screen.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TapeTokens {
    /// This row's own tokens sent/received, once known (a finished
    /// or running prompt/tool) -- `None` for a row ahead with
    /// nothing of its own yet.
    pub sent: Option<u64>,
    pub received: Option<u64>,
    /// History's own median for this exact row (§3.5's "usually"),
    /// pooled across loop passes the same way `StepStats` already is
    /// -- `None` below the 3-run threshold.
    pub usual: Option<u64>,
    /// Set on the run's single largest finished step -- the one bold
    /// figure in the column (§3.5).
    pub is_largest: bool,
    /// A loop row's own sparkline: one entry per pass that has a
    /// token figure of its own yet, oldest first.
    pub loop_pass_tokens: Vec<u64>,
    /// `sent`/`received` are a running prompt's own live estimate,
    /// not its final figure (issue #675) -- the bar and the count
    /// draw in the faint "live" style, settling once the row's own
    /// `sent`/`received` next read the finished, exact pair instead.
    pub live: bool,
}

impl TapeTokens {
    pub fn total(&self) -> Option<u64> {
        match (self.sent, self.received) {
            (None, None) => None,
            (sent, received) => Some(sent.unwrap_or(0) + received.unwrap_or(0)),
        }
    }
}

/// A row's own "what it did or will do" (§3.4) -- already formatted
/// text, so a front end never has to know a leaf's own kind to draw
/// it; `render::details_for`'s own field order feeds every variant.
#[derive(Debug, Clone, PartialEq)]
pub struct TapeRow {
    pub path: String,
    /// The step name, its loop prefix dropped when a loop row of its
    /// own already carries it (§3.4: `iter_0 › draft`, never
    /// `drafts.iter_0 › draft`).
    pub label: String,
    pub kind: RowKind,
    pub status: RowStatus,
    /// The event clock, shown on every row except one ahead (§3.4's
    /// anatomy: "the event clock… the status glyph…").
    pub ts: Option<String>,
    /// What this row did, is doing, or will do -- `↑212 ↓3 · Bug.`,
    /// `3.0s · sleep 3`, `kind != ""`, `3 passes · 2 wide · 0 done`,
    /// `prompt · queued`, `skipped · not reached · prompt`.
    pub what: String,
    /// The faint preview line under a prompt row, when previews are
    /// on -- the text actually sent once finished, the plan's own
    /// unrendered template while still ahead.
    pub preview: Option<String>,
    /// Bold: the running rows below the Now band (§3.4).
    pub bold: bool,
    pub tokens: TapeTokens,
}

/// A non-row line in the Tape's own list (§2.1): the bookends, the
/// Now band and the slots rule.
#[derive(Debug, Clone, PartialEq)]
pub enum TapeItem {
    RunStarted {
        ts: Option<String>,
        document: String,
        engine: String,
    },
    Row(TapeRow),
    NowBand {
        ts: Option<String>,
        elapsed_s: f64,
    },
    SlotsRule {
        busy: u64,
        slots: u64,
        queued: u64,
    },
    /// The Starting/Ended/Failed head rule (§2.1), drawn as a full-
    /// width rule in the head's own colour (read straight off
    /// [`TapeScreen::head`], never carried here) -- replaces the Now
    /// band in every state but `Normal`, where it is never pushed.
    HeadRule,
    RunEnd {
        usual_total_tokens: Option<u64>,
        usual_wall_s: Option<f64>,
        eta_ts: Option<String>,
    },
}

/// Screen 0's own model (issue #678, replacing the old Overview): the
/// Tape's list, top to bottom, plus the counts and head rule its own
/// title and top border need, and the selected row's own detail (the
/// same [`FocusedStep`] the Flow screen already builds
/// theirs from).
#[derive(Debug, Clone, PartialEq)]
pub struct TapeScreen {
    /// Tokens per bar cell, derived from history (§3.5) -- `50`
    /// without enough of it.
    pub unit: u64,
    pub items: Vec<TapeItem>,
    pub done: u64,
    pub running: u64,
    pub ahead: u64,
    pub head: TapeHead,
    pub selected: Option<FocusedStep>,
    /// The selected row's own tape-style breadcrumb (`gate › reply`,
    /// not [`FocusedStep::breadcrumb`]'s plain `reply`) -- the detail
    /// pane's own title while something's selected (§2.1: "gate ›
    /// reply · Selected").
    pub selected_breadcrumb: Option<String>,
    /// The selected row's own usual tokens and the run count behind
    /// it (§2.1's detail pane: "265 t +47% · median of 3 runs") --
    /// `None` below the 3-run threshold.
    pub selected_usual: Option<(u64, usize)>,
    /// The selected row's own share of the run's tokens so far, as a
    /// whole percent (§2.1's "45% of this run so far").
    pub selected_share_pct: Option<u64>,
    /// The selected row's own content the detail pane's own prompt
    /// state needs beyond [`FocusedStep`] (issue #678's §2.1 detail
    /// pane: Tokens' own sent/received split, and the prompt sent/
    /// reply blocks in full, never [`FocusedStep::detail`]'s own
    /// truncated snippet) -- `None` for a non-prompt row, or with
    /// nothing selected at all.
    pub selected_detail: Option<TapeSelectedDetail>,
    /// The Starting state's own detail pane (§2.1): `None` outside
    /// [`TapeHead::Starting`].
    pub starting_detail: Option<TapeStartingDetail>,
    /// The Ended state's own detail pane (§2.1): `None` outside
    /// [`TapeHead::Ended`].
    pub ended_detail: Option<TapeEndedDetail>,
    /// The Failed state's own detail pane (§2.1): `None` outside
    /// [`TapeHead::Failed`].
    pub failed_detail: Option<TapeFailedDetail>,
    /// The Totals row's own "usual" deviation (§3.3): the sum of
    /// every row that has run or is running so far, its own usual
    /// tokens -- `None` when none of them has one.
    pub usual_so_far_tokens: Option<u64>,
}

/// The selected row's own prompt content (issue #678's §2.1 detail
/// pane), beyond what [`FocusedStep`] already carries for Flow.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TapeSelectedDetail {
    /// The row's own finish instant, for the Status line's clock
    /// (`FocusedStep::elapsed_s` already has its own duration).
    pub finished_ts: Option<String>,
    pub tokens_sent: Option<u64>,
    pub tokens_received: Option<u64>,
    pub prompt_sent: Option<String>,
    pub prompt_sent_tokens: Option<u64>,
    pub reply: Option<String>,
    pub reply_tokens: Option<u64>,
    /// A running prompt's own live figures (issue #675) -- `None`
    /// once it has finished (`tokens_sent`/`tokens_received` above
    /// carry its final figures instead by then) or when it never had
    /// any live record at all (shown as today, never a crash).
    pub live: Option<crate::model::PromptLiveFigures>,
}

/// The Starting state's own detail pane content (§2.1: "the detail
/// pane shows Engine, Plan ..., Shape, Usual, ETA if usual, and the
/// first step").
#[derive(Debug, Clone, PartialEq)]
pub struct TapeStartingDetail {
    pub engine: String,
    /// `14 effects · 6 prompts · 5 tools`.
    pub plan_text: String,
    /// `1 decision · 3-pass loop · 2 wide`.
    pub shape_text: String,
    pub usual_total_tokens: Option<u64>,
    pub usual_total_s: Option<f64>,
    pub usual_count: Option<usize>,
    pub eta_ts: Option<String>,
    pub first_step_label: String,
    /// `prompt · scripted-model`.
    pub first_step_type: String,
    pub first_step_usual: Option<u64>,
}

/// The Ended state's own detail pane content (§2.1): Outcome,
/// Tokens, Usual, Effects, Decision, Largest, the handled failure's
/// error and the run's summary value.
#[derive(Debug, Clone, PartialEq)]
pub struct TapeEndedDetail {
    pub ok: bool,
    pub duration_s: f64,
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub usual_total_tokens: Option<u64>,
    pub usual_dev_pct: Option<f64>,
    pub usual_total_s: Option<f64>,
    pub effects_done: u64,
    pub effects_planned: Option<u64>,
    pub handled_count: usize,
    /// `gate → then`, the first decision taken.
    pub decision_text: Option<String>,
    pub largest_label: Option<String>,
    pub largest_tokens: Option<u64>,
    pub handled_failure: Option<TapeHandledFailureDetail>,
    /// The run's own summary value (§2.1): the last finished leaf's
    /// own display value.
    pub summary: Option<String>,
}

/// A handled failure's own detail-pane block (issue #678's Ended
/// state): `exit 1 · on_error: continue · the run went on`, the
/// error in full.
#[derive(Debug, Clone, PartialEq)]
pub struct TapeHandledFailureDetail {
    pub label: String,
    /// `tool · shell · ls /nonexistent-osi-demo`.
    pub what: String,
    /// `exit 1 · on_error: continue · the run went on`.
    pub exit_on_error: String,
    pub error: String,
}

/// The Failed state's own detail pane content (§2.1): the error in
/// full and the list of what was not reached.
#[derive(Debug, Clone, PartialEq)]
pub struct TapeFailedDetail {
    pub duration_s: f64,
    pub start_ts: Option<String>,
    pub failing_label: String,
    /// `tool · shell`.
    pub failing_type: String,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub usual_tokens: Option<u64>,
    pub usual_dev_pct: Option<f64>,
    pub error: String,
    /// `gate → then or else`, `drafts · 3 passes`, `flaky`, ... in
    /// plan order.
    pub not_reached: Vec<String>,
}

/// §3.5's unit rule: the smallest of these that keeps `largest`'s
/// own drawn bar inside eight cells, `50` with no `largest` to go on
/// at all (no history, or nothing finished yet). "Inside eight
/// cells" means by the same rounding the bar itself draws with
/// (`tape_token_bar`'s round-half-up, capped at eight) -- 402 at a
/// 50-token unit rounds to a full 8-cell bar, not 9, so the unit
/// stays 50 for it.
pub(crate) fn tape_unit(largest_usual: Option<u64>) -> u64 {
    const STEPS: [u64; 6] = [10, 20, 50, 100, 200, 500];
    let Some(largest) = largest_usual else {
        return 50;
    };
    STEPS
        .into_iter()
        .find(|&unit| (largest as f64 / unit as f64).round() <= 8.0)
        .unwrap_or(500)
}

/// A tool row's own "what" text once finished or running: `3.0s ·
/// sleep 3` -- duration, then the rendered command and its args,
/// straight from `meta.params_rendered` (the same field `render::
/// details_for`'s own `meta_summary` reads, but read here directly
/// off the already-flattened node so a row's own text costs no more
/// than this one lookup -- `details_for` re-flattens the whole state
/// on every call, which a per-row loop here must never pay for).
fn tape_tool_what(row: &Row, node: Option<&NodeMeta>) -> String {
    let command = node.and_then(|n| {
        let command = n.meta.pointer("/params_rendered/command")?.as_str()?;
        let args = n
            .meta
            .pointer("/params_rendered/args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .map(crate::render::display_value)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|a| !a.is_empty());
        Some(match args {
            Some(args) => format!("{command} {args}"),
            None => command.to_string(),
        })
    });
    match (row.duration_s, command) {
        (Some(d), Some(cmd)) => format!("{} · {cmd}", crate::format::format_duration(d)),
        (Some(d), None) => crate::format::format_duration(d),
        (None, Some(cmd)) => cmd,
        (None, None) => String::new(),
    }
}

/// A tool's own exit code, read out of its error text (`"/bin/ls
/// failed (exit 1): ..."`) rather than `meta.exit_code` -- the shell
/// adapter leaves that field `null` on exactly the failures that
/// carry one in the message instead (issue #678's Ended/Failed
/// states: a handled failure's own row, `exit 1 · handled ·
/// on_error: continue`). `None` when the text has no such substring.
fn tape_exit_code(error: &str) -> Option<&str> {
    let after = error.split_once("(exit ")?.1;
    after.split(')').next()
}

/// A failed step's own `on_error:` word (issue #678's Ended/Failed
/// states), the same plan lookup `diff::on_error_suffix` already
/// makes for the log -- empty for `on_error: fail` (the default,
/// never shown) or a path with no plan entry at all.
fn tape_on_error_word(plan: &PlanTree, path: &str) -> &'static str {
    match plan
        .match_path(path)
        .and_then(|m| m.entries.first().map(|e| e.on_error))
    {
        Some(electricity_bytecode::OnError::Continue) => "continue",
        Some(electricity_bytecode::OnError::Skip) => "skip",
        _ => "",
    }
}

/// A finished prompt's own "what" text: `↑212 ↓3 · Bug.` -- sent,
/// received, then the reply's own first words (§3.4).
fn tape_prompt_what(sent: u64, received: u64, reply_first_words: Option<&str>) -> String {
    let arrows = format!(
        "↑{} ↓{}",
        crate::format::format_tokens_thousands(sent),
        crate::format::format_tokens_thousands(received)
    );
    match reply_first_words {
        Some(words) if !words.is_empty() => format!("{arrows} · {words}"),
        _ => arrows,
    }
}

/// A running prompt's own "what" text (issue #675): the elapsed
/// time and, on a retry, its own attempt number, first -- the
/// row's own `what` column is cut from the right the moment the
/// pane is too narrow for the rest of it (`tape_what_text`), so
/// whatever comes after this point is the first thing to go. After
/// that: for a streaming adapter, `waiting for the first token`
/// before its own first delta, else how much has come back so far
/// (exact once the provider has reported it, `≈` while it's still
/// an estimate) and the rate, once there's been enough of a second
/// delta to measure one. A non-streaming adapter has none of that to
/// show, only the elapsed time and attempt (today's own tool-row
/// shape).
fn tape_running_prompt_what(
    duration_s: Option<f64>,
    live: &crate::model::PromptLiveFigures,
) -> String {
    // This attempt's own elapsed (`live.elapsed_s`, since its own
    // `started_at`) wins over the row's own `duration_s` (since the
    // first attempt's `created_at`, never reset by a retry) -- the
    // figure right after it (`waiting for the first token`, or the
    // received count and rate) is always this attempt's own, so the
    // elapsed next to it must read the same attempt's clock, not one
    // that also counts an earlier attempt's own backoff.
    let elapsed = live
        .elapsed_s
        .or(duration_s)
        .map(crate::format::format_duration)
        .unwrap_or_default();
    let mut what = elapsed;
    if let Some(attempt) = live.attempt.filter(|&a| a > 0) {
        what.push_str(&format!(" · attempt {}", attempt + 1));
    }
    if !live.streaming {
        return what;
    }
    if live.waiting_for_first_token {
        what.push_str(" · waiting for the first token");
    } else if let Some((n, is_estimate)) = live.tokens_received_or_estimate() {
        let marker = if is_estimate { "≈" } else { "" };
        let rate = live
            .tokens_per_second
            .map(|r| format!(" · {} t/s", r.round() as i64))
            .unwrap_or_default();
        what.push_str(&format!(
            " · ↓{marker}{} t{rate}",
            crate::format::format_tokens_thousands(n)
        ));
    }
    what
}

/// A running prompt's own token column (issue #675): sent/received
/// read exact-or-estimate the same way the row text does, marked
/// `live` so the bar and the figure draw in the faint style until
/// the row's own tokens next read the finished, exact pair instead.
fn tape_running_prompt_tokens(live: &crate::model::PromptLiveFigures) -> TapeTokens {
    let sent = live.tokens_sent_or_estimate();
    let received = live.tokens_received_or_estimate();
    TapeTokens {
        sent: sent.map(|(n, _)| n),
        received: received.map(|(n, _)| n),
        live: sent.is_some() || received.is_some(),
        ..Default::default()
    }
}

/// §3.4's "first words": the reply's own text, cut at the first
/// sentence end or a fixed character budget, whichever comes first
/// -- `Bug.`, `Thanks for the ⋯` (never the whole reply, which the
/// detail pane shows in full). §3.8's row-text ellipsis (`⋯`), not
/// a sentence's own `…`: every Tape row cut short, a preview line
/// included, uses the same glyph.
fn first_words(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if let Some(end) = text.find(['.', '!', '?']) {
        let cut = &text[..=end];
        if cut.chars().count() <= max_chars {
            return cut.to_string();
        }
    }
    let truncated: String = text.chars().take(max_chars).collect();
    if truncated.chars().count() < text.chars().count() {
        format!("{truncated}⋯")
    } else {
        truncated
    }
}

/// A row's own "usually" tokens (§3.5), tried first by its exact
/// loop pass (`drafts.iter_2.draft`'s own three recorded figures,
/// never summed with its sibling passes) and, failing that, by the
/// pass-stripped key every other step already uses -- that key's own
/// `tokens_p50` is already a single typical pass's figure, not the
/// whole loop's (`history::compute_stats` divides each run's own sum
/// by that run's own pass count before taking the median across
/// runs), so the fallback never over-counts for a loop pass still
/// missing its own literal history. Every row on the Tape goes
/// through this, not only the ones ahead, so a finished or running
/// row's `+47%` deviation has the same figure behind it as a queued
/// one's `usually 155 t`.
pub(crate) fn tape_row_usual_tokens(
    path: &str,
    plan: &PlanTree,
    header: &crate::render::Header,
) -> Option<u64> {
    let literal = crate::history::display_key_literal(path, plan)
        .and_then(|key| header.history_step_usual.get(&key))
        .and_then(|s| s.tokens_p50);
    let usual = literal.or_else(|| {
        crate::history::display_key(path, plan)
            .and_then(|key| header.history_step_usual.get(&key))
            .and_then(|s| s.tokens_p50)
    });
    // A step with no token field at all (a tool, a decision, a bare
    // loop container) still gets a `StepStats` entry (count, busy
    // time) -- but its own `tokens_p50` is a literal `0`, every
    // recorded pass's own default, never a real usual to show
    // (`usually 0 t`, which no frame shows, next to a tool's own
    // duration).
    usual.filter(|&t| t > 0)
}

/// Whether `path` (an ahead row, or one of its own leaves) is a
/// loop pass waiting for a free concurrency slot (§3.4's " ·
/// queued"): some path segment, the pass's own or any of its
/// steps', is `iter_N`, same shape every pass placeholder resolves
/// to.
fn tape_row_is_under_a_queued_pass(path: &str) -> bool {
    path.split('.').any(|seg| {
        seg.strip_prefix("iter_")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// An `if`'s own condition text (§2.1: `gate → then` followed by its
/// condition, dim) -- `None` for anything but a named `if` the plan
/// actually has a condition for.
fn tape_condition_text(plan: &PlanTree, path: &str) -> Option<String> {
    let entry = plan.match_path(path)?.entries.first()?;
    match &entry.kind {
        crate::plan::PlanEntryKind::If { condition, .. } => {
            condition.as_ref().map(|c| shorten_condition_text(&c.text))
        }
        _ => None,
    }
}

/// A CEL condition's own `state.<path>.value` noise, dropped for the
/// Tape's row (§2.1: `gate → then` reads `kind != ""`, never
/// `state.prime.kind.value != ""`) -- the Flow screen still shows the
/// expression verbatim, so this is the Tape's own transform, not a
/// change to `ConditionInfo::text` itself.
fn shorten_condition_text(text: &str) -> String {
    text.replace("state.prime.", "").replace(".value", "")
}

/// A pending loop's own " · each topic · 3 passes" suffix (§2.1's
/// Starting state: `drafts` reads `loop · each topic · 3 pass⋯`
/// before any event at all) -- empty once either half is unknown
/// (an unnamed loop has no `each_as` of its own to read; a `while`
/// loop, or one whose `each.in` doesn't resolve against `state` yet,
/// has no total).
fn tape_loop_each_text(
    plan: &PlanTree,
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    state: Option<&Value>,
) -> String {
    let each_as = plan.match_path(path).and_then(|m| {
        m.entries.iter().find_map(|e| match &e.kind {
            crate::plan::PlanEntryKind::Loop {
                each_as: Some(a), ..
            } => Some(a.as_str()),
            _ => None,
        })
    });
    let Some(each_as) = each_as else {
        return String::new();
    };
    let total = flat
        .get(path)
        .and_then(|n| n.progress_total)
        .or_else(|| state.and_then(|root| plan.resolve_each_len(path, root)));
    match total {
        Some(total) => format!(" · each {each_as} · {total} passes"),
        None => format!(" · each {each_as}"),
    }
}

/// A prompt row's own preview line (§2.1: the faint `› …` line under
/// a prompt), truncated to a plain budget -- a front end still clips
/// it further to the pane's own width, same as any other row text,
/// but this keeps a single preview from being an unreasonably long
/// string regardless.
const TAPE_PREVIEW_CHARS: usize = 120;

fn tape_preview_line(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // A plain character budget, never `first_words`'s own "stop at
    // the first sentence" rule -- a prompt template routinely has
    // an embedded period well before its own end (a value like
    // "kind: Bug." ends mid-phrase), which would cut a preview
    // there instead of near this budget's own, much wider edge.
    let truncated: String = collapsed.chars().take(TAPE_PREVIEW_CHARS).collect();
    let body = if truncated.chars().count() < collapsed.chars().count() {
        format!("{truncated}⋯")
    } else {
        truncated
    };
    format!("› {body}")
}

/// Builds one row's own `(what, preview, tokens)` (§3.4/§3.5).
/// `queued` is §3.4's " · queued" suffix for a row ahead that is a
/// loop pass waiting on a busy loop's own concurrency.
#[allow(clippy::too_many_arguments)]
fn tape_row_content(
    row: &Row,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    previews_on: bool,
    queued: bool,
    now: &str,
) -> (String, Option<String>, TapeTokens) {
    let node = flat.get(&row.path);
    let pending = row.status.kind == StatusKind::Pending;
    let skipped = matches!(row.status.kind, StatusKind::Skipped);
    let queued_suffix = if queued { " · queued" } else { "" };

    match row.kind {
        RowKind::Prompt | RowKind::Reflector => {
            if pending {
                let preview = previews_on
                    .then(|| plan.prompt_template(&row.path))
                    .flatten()
                    .map(|t| tape_preview_line(&t));
                (
                    format!("prompt{queued_suffix}"),
                    preview,
                    TapeTokens::default(),
                )
            } else if skipped {
                (
                    "skipped · not reached · prompt".to_string(),
                    None,
                    TapeTokens::default(),
                )
            } else {
                let is_running = matches!(
                    row.status.kind,
                    StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                );
                let preview = if previews_on {
                    node.and_then(|n| n.meta.get("prompt_sent"))
                        .and_then(Value::as_str)
                        .map(tape_preview_line)
                } else {
                    None
                };
                if is_running {
                    match crate::model::prompt_live_figures(node, &row.path, model, now) {
                        Some(live) => {
                            let what = tape_running_prompt_what(row.duration_s, &live);
                            let tokens = tape_running_prompt_tokens(&live);
                            (what, preview, tokens)
                        }
                        // No live figures at all (R4: malformed or
                        // missing `meta.live`, never a crash) -- the
                        // elapsed time alone, the same shape a
                        // non-streaming tool's own running row reads.
                        None => (
                            row.duration_s
                                .map(crate::format::format_duration)
                                .unwrap_or_default(),
                            preview,
                            TapeTokens::default(),
                        ),
                    }
                } else {
                    let (sent, received) = node
                        .map(|n| crate::model::node_tokens(&n.meta))
                        .unwrap_or((0, 0));
                    let reply_words = node
                        .map(|n| crate::render::display_value(&n.value))
                        .filter(|v| !v.is_empty())
                        .map(|v| first_words(&v, 24));
                    let what = tape_prompt_what(sent, received, reply_words.as_deref());
                    let tokens = TapeTokens {
                        sent: Some(sent),
                        received: Some(received),
                        ..Default::default()
                    };
                    (what, preview, tokens)
                }
            }
        }
        RowKind::If => {
            let branch = node.and_then(|n| n.branch.clone());
            let condition = tape_condition_text(plan, &row.path);
            let condition_suffix = condition
                .as_deref()
                .map(|c| format!(" · {c}"))
                .unwrap_or_default();
            match branch {
                Some(_) => (condition.unwrap_or_default(), None, TapeTokens::default()),
                None if pending => (format!("if{condition_suffix}"), None, TapeTokens::default()),
                None if skipped => (
                    format!("skipped · not reached · if{condition_suffix}"),
                    None,
                    TapeTokens::default(),
                ),
                None => ("if".to_string(), None, TapeTokens::default()),
            }
        }
        RowKind::Loop => {
            if skipped {
                (
                    "skipped · not reached · loop".to_string(),
                    None,
                    TapeTokens::default(),
                )
            } else if pending {
                let each = tape_loop_each_text(plan, &row.path, flat, state);
                (format!("loop{each}"), None, TapeTokens::default())
            } else {
                let done = row.loop_progress.map(|p| p.done).unwrap_or(0);
                let total = row.loop_progress.and_then(|p| p.total);
                let slots = model
                    .running_or_queued_bound(&row.path)
                    .unwrap_or(total.unwrap_or(0));
                let total_text = total
                    .map(|t| format!("{t} passes"))
                    .unwrap_or_else(|| "? passes".to_string());
                let what = format!("{total_text} · {slots} wide · {done} done");
                // The sparkline: one bar per pass index that has a
                // token figure of its own so far, oldest first -- one
                // pass over `flat`, grouped by index, rather than a
                // fresh O(flat.len()) scan per index (issue #678: a
                // 2,000-pass loop must still redraw well under the
                // redraw interval).
                let prefix = format!("{}.iter_", row.path);
                let mut per_pass: std::collections::BTreeMap<u64, (u64, u64)> =
                    std::collections::BTreeMap::new();
                for (path, node) in flat {
                    let Some(rest) = path.strip_prefix(&prefix) else {
                        continue;
                    };
                    let Some(Ok(idx)) = rest.split('.').next().map(str::parse) else {
                        continue;
                    };
                    let (sent, received) = crate::model::node_tokens(&node.meta);
                    let entry = per_pass.entry(idx).or_insert((0, 0));
                    entry.0 += sent;
                    entry.1 += received;
                }
                let tokens = TapeTokens {
                    loop_pass_tokens: per_pass
                        .values()
                        .filter(|(s, r)| s + r > 0)
                        .map(|(s, r)| s + r)
                        .collect(),
                    ..Default::default()
                };
                (what, None, tokens)
            }
        }
        RowKind::Tool | RowKind::Use | RowKind::Yield | RowKind::Dynamic | RowKind::TryFinally => {
            if pending {
                // §2.1's Starting state: a tool ahead reads its own
                // static command, when every part of it is a plain
                // literal (issue #678), not just its bare kind.
                let command = (row.kind == RowKind::Tool)
                    .then(|| plan.match_path(&row.path))
                    .flatten()
                    .and_then(|m| m.entries.first())
                    .and_then(|e| e.tool_command.clone());
                let what = match command {
                    Some(command) => format!("{} · {command}{queued_suffix}", row.kind.label()),
                    None => format!("{}{queued_suffix}", row.kind.label()),
                };
                (what, None, TapeTokens::default())
            } else if skipped {
                (
                    format!("skipped · not reached · {}", row.kind.label()),
                    None,
                    TapeTokens::default(),
                )
            } else if row.status.kind == StatusKind::Failed {
                let error = node.and_then(|n| n.error.clone()).unwrap_or_default();
                (format!("✗ {error}"), None, TapeTokens::default())
            } else if row.status.kind == StatusKind::FailedHandled {
                let error = node.and_then(|n| n.error.clone()).unwrap_or_default();
                let exit = tape_exit_code(&error)
                    .map(|c| format!("exit {c} · "))
                    .unwrap_or_default();
                let mode = tape_on_error_word(plan, &row.path);
                let mode_text = if mode.is_empty() {
                    String::new()
                } else {
                    format!(" · on_error: {mode}")
                };
                (
                    format!("{exit}handled{mode_text}"),
                    None,
                    TapeTokens::default(),
                )
            } else {
                (tape_tool_what(row, node), None, TapeTokens::default())
            }
        }
        RowKind::Root
        | RowKind::Pass
        | RowKind::QuickSteps
        | RowKind::Report
        | RowKind::Unknown => (String::new(), None, TapeTokens::default()),
    }
}

/// A step's own full label for the Tape's list (§3.4): an `if`'s own
/// row carries its branch (`gate → then`); everything else keeps
/// `render::Row::label` as-is (already relative to its parent, and
/// a loop pass's own direct child is already named `iter_0 ›
/// draft` by `fold`/`render`, never repeating the loop's own name).
fn tape_row_label(row: &Row, flat: &BTreeMap<String, NodeMeta>, plan: &PlanTree) -> String {
    if row.kind == RowKind::If {
        if let Some(branch) = flat.get(&row.path).and_then(|n| n.branch.clone()) {
            return format!("{} → {branch}", row.label);
        }
    }
    // §3.4: "a loop pass's own direct child" (`iter_0 › draft`) and an
    // `if`'s own taken-branch child (`gate › reply`) carry their own
    // parent's name -- generic depth-based folding only joins a
    // container with its *one* child, which a loop pass with more
    // than one body effect (`draft` then `pause`) never is, so this
    // reads the parent straight off the row's own path instead.
    let Some((parent_path, own_name)) = row.path.rsplit_once('.') else {
        return row.label.clone();
    };
    let parent_name = parent_path.rsplit('.').next().unwrap_or(parent_path);
    let is_pass = parent_name.len() > 5
        && parent_name.starts_with("iter_")
        && parent_name[5..].bytes().all(|b| b.is_ascii_digit());
    let parent_is_if = plan
        .match_path(parent_path)
        .and_then(|m| m.entries.first())
        .is_some_and(|entry| matches!(entry.kind, crate::plan::PlanEntryKind::If { .. }));
    // A sequential container (`use`, a chain-flow `dynamic`, a
    // `try`/`finally`) inlines its own children straight onto the
    // rail (`rail::build_level`'s own doc comment) -- its child
    // still carries this same parent breadcrumb everywhere a plain
    // step's own name is shown on its own, the Flow screen's
    // Selected pane and status line included. The document root
    // itself compiles to the very same `Dynamic { flow: Chain }`
    // shape its own top-level block is, so it's excluded here by
    // its own path having no parent of its own -- otherwise every
    // top-level step in every document would read "prime › x".
    let parent_is_sequential_container = parent_path.contains('.')
        && plan
            .match_path(parent_path)
            .and_then(|m| m.entries.first())
            .is_some_and(|entry| {
                matches!(
                    entry.kind,
                    PlanEntryKind::Leaf(LeafEffectKind::Use) | PlanEntryKind::TryFinally
                ) || matches!(&entry.kind, PlanEntryKind::Dynamic { flow: Flow::Chain })
            });
    if is_pass || parent_is_if || parent_is_sequential_container {
        format!("{parent_name} › {own_name}")
    } else {
        row.label.clone()
    }
}

/// §3.5's unit rule applied to the run's own largest *actual* step
/// so far, for the bold figure, and separately its largest *usual*
/// step, for the bar's own scale.
fn tape_largest_tokens(items: &[TapeItem]) -> Option<u64> {
    items
        .iter()
        .filter_map(|item| match item {
            TapeItem::Row(row) => row.tokens.total(),
            _ => None,
        })
        .max()
}

/// [`TapeSelectedDetail`]: a selected prompt row's own sent/received
/// split and its prompt/reply in full -- `Default` (every field
/// `None`) for any other kind, which still carries the row's own
/// finish instant.
fn build_tape_selected_detail(
    row: &Row,
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    now: &str,
) -> TapeSelectedDetail {
    let node = flat.get(&row.path);
    let finished_ts = node.and_then(|n| n.completed_at.clone());
    if !matches!(row.kind, RowKind::Prompt | RowKind::Reflector) {
        let (sent, received) = node
            .map(|n| crate::model::node_tokens(&n.meta))
            .unwrap_or((0, 0));
        return TapeSelectedDetail {
            finished_ts,
            tokens_sent: (sent + received > 0).then_some(sent),
            tokens_received: (sent + received > 0).then_some(received),
            ..Default::default()
        };
    }
    let is_running = matches!(
        row.status.kind,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
    );
    // A running prompt's own `tokens_sent`/`tokens_received` are
    // always `null` (DESIGN.md), so `node_tokens` would otherwise
    // report a bogus `0`/`0` here -- `None` instead, so the Tokens
    // line simply doesn't draw until the row actually finishes; the
    // Live block below is what a running prompt's detail pane shows
    // instead.
    let (sent, received) = if is_running {
        (None, None)
    } else {
        let (sent, received) = node
            .map(|n| crate::model::node_tokens(&n.meta))
            .unwrap_or((0, 0));
        (Some(sent), Some(received))
    };
    let live = is_running
        .then(|| crate::model::prompt_live_figures(node, &row.path, model, now))
        .flatten();
    let prompt_sent = node
        .and_then(|n| n.meta.get("prompt_sent"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let reply = node
        .map(|n| crate::render::display_value(&n.value))
        .filter(|v| !v.is_empty());
    TapeSelectedDetail {
        finished_ts,
        tokens_sent: sent,
        tokens_received: received,
        prompt_sent,
        prompt_sent_tokens: sent,
        live,
        reply,
        reply_tokens: received,
    }
}

/// A named loop's own "N-pass loop · M wide" (issue #678's Starting
/// Shape line and Failed Not-Reached list) -- `None` when its own
/// pass count doesn't resolve yet (neither a state node nor a static
/// `each.in` against `state`).
fn tape_loop_shape_text(
    path: &str,
    plan: &PlanTree,
    flat: &BTreeMap<String, NodeMeta>,
    state: Option<&Value>,
    max_concurrency: Option<u32>,
) -> Option<String> {
    let total = flat
        .get(path)
        .and_then(|n| n.progress_total)
        .or_else(|| state.and_then(|root| plan.resolve_each_len(path, root)))?;
    let wide = max_concurrency.unwrap_or(total as u32);
    Some(format!("{total}-pass loop · {wide} wide"))
}

/// [`TapeStartingDetail`] (issue #678's §2.1 Starting state): Plan's
/// own effect/prompt/tool counts come from `plan.countable_paths()`,
/// each path's own pass count resolved the same way `effects_planned`
/// is (`render::leaf_pass_count`) -- the loop whose `each.in` is
/// already known from `state`'s own input, before any event at all.
fn build_tape_starting_detail(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    header: &crate::render::Header,
    state: Option<&Value>,
) -> TapeStartingDetail {
    let mut total = 0u64;
    let mut prompts = 0u64;
    let mut tools = 0u64;
    let mut decisions = 0u64;
    let mut loop_texts = Vec::new();
    // Deduplicated the same way `render::build_header`'s own
    // `effects_planned` is -- `countable_paths()` can repeat an
    // entry (a chain op reached more than one way).
    let countable_paths: std::collections::BTreeSet<&String> =
        plan.countable_paths().iter().collect();
    for path in countable_paths {
        let (passes, _) = crate::render::leaf_pass_count(path, plan, flat, state);
        total = total.saturating_add(passes);
        // `match_path` only ever resolves a concrete `iter_N` segment
        // (DESIGN.md §5's path-display ask renders the *template*
        // path's own placeholder as the literal text `"iter_*"`,
        // which is never a real pass index) -- `"iter_0"` stands in
        // for it here since the entries at any one pass index are the
        // same ones at every other.
        let concrete_path = path.replace("iter_*", "iter_0");
        let Some(entry) = plan
            .match_path(&concrete_path)
            .and_then(|m| m.entries.first())
        else {
            continue;
        };
        match &entry.kind {
            PlanEntryKind::Leaf(LeafEffectKind::Prompt | LeafEffectKind::Reflector) => {
                prompts += passes;
            }
            PlanEntryKind::Leaf(
                LeafEffectKind::Tool | LeafEffectKind::Use | LeafEffectKind::Yield,
            ) => {
                tools += passes;
            }
            PlanEntryKind::If { named: true, .. } => decisions += 1,
            PlanEntryKind::Loop {
                named: true,
                max_concurrency,
                ..
            } => {
                if let Some(text) = tape_loop_shape_text(path, plan, flat, state, *max_concurrency)
                {
                    loop_texts.push(text);
                }
            }
            _ => {}
        }
    }
    let plan_text = format!("{total} effects · {prompts} prompts · {tools} tools");
    let mut shape_parts = Vec::new();
    if decisions > 0 {
        let word = if decisions == 1 {
            "decision"
        } else {
            "decisions"
        };
        shape_parts.push(format!("{decisions} {word}"));
    }
    shape_parts.extend(loop_texts);
    let shape_text = shape_parts.join(" · ");

    let first_path = plan.leaf_paths().first();
    let first_entry = first_path
        .and_then(|p| plan.match_path(p))
        .and_then(|m| m.entries.first());
    let first_step_label = first_path
        .and_then(|p| rows.iter().find(|r| &r.path == p))
        .map(|r| tape_row_label(r, flat, plan))
        .or_else(|| first_path.map(|p| p.rsplit('.').next().unwrap_or(p).to_string()))
        .unwrap_or_default();
    // A step's own explicit `model:`/`adapter:` wins; failing that,
    // the run's own effective default (issue #678's Starting state:
    // known from config before any event, the same `runtime.
    // effective_settings` a real engine's own live state already
    // carries for exactly this reason).
    let effective_model = state
        .and_then(|s| s.pointer("/runtime/effective_settings/model"))
        .and_then(Value::as_str);
    let first_step_type = match first_entry {
        Some(entry) => {
            let kind_label = RowKind::from_plan_entry(&entry.kind).label();
            match entry
                .model
                .as_deref()
                .or(entry.provider.as_deref())
                .or(effective_model)
            {
                Some(m) => format!("{kind_label} · {m}"),
                None => kind_label.to_string(),
            }
        }
        None => String::new(),
    };
    let first_step_usual = first_path.and_then(|p| tape_row_usual_tokens(p, plan, header));

    TapeStartingDetail {
        engine: header.engine.clone(),
        plan_text,
        shape_text,
        usual_total_tokens: header.history_usual_total_tokens,
        usual_total_s: header.history_usual_total_s,
        usual_count: header.history_same_hash_count,
        eta_ts: header.history_eta_ts.clone(),
        first_step_label,
        first_step_type,
        first_step_usual,
    }
}

/// [`TapeEndedDetail`] (issue #678's §2.1 Ended state).
fn build_tape_ended_detail(
    ok: bool,
    handled_failure: Option<(String, String)>,
    items: &[TapeItem],
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    header: &crate::render::Header,
) -> TapeEndedDetail {
    let usual_total_tokens = header.history_usual_total_tokens;
    let total_tokens = header.tokens_sent + header.tokens_received;
    let usual_dev_pct = usual_total_tokens
        .filter(|&u| u > 0)
        .map(|u| ((total_tokens as f64 - u as f64) / u as f64) * 100.0);
    let decision_text = rows
        .iter()
        .find(|r| r.kind == RowKind::If)
        .filter(|r| flat.get(&r.path).is_some_and(|n| n.branch.is_some()))
        .map(|r| tape_row_label(r, flat, plan));
    let largest = items.iter().find_map(|item| match item {
        TapeItem::Row(row) if row.tokens.is_largest => {
            Some((row.label.clone(), row.tokens.total().unwrap_or(0)))
        }
        _ => None,
    });
    let handled_count = rows
        .iter()
        .filter(|r| r.status.kind == StatusKind::FailedHandled)
        .count();
    let handled_failure_detail = handled_failure.and_then(|(label, error)| {
        let row = rows.iter().find(|r| r.label == label)?;
        let node = flat.get(&row.path);
        let provider = node
            .and_then(|n| n.meta.get("provider"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let command = tape_tool_what(row, node);
        let what = format!(
            "{} · {provider} · {}",
            row.kind.label(),
            command
                .rsplit_once("· ")
                .map(|(_, c)| c.trim())
                .unwrap_or(&command)
        );
        let exit = tape_exit_code(&error)
            .map(|c| format!("exit {c} · "))
            .unwrap_or_default();
        let mode = tape_on_error_word(plan, &row.path);
        let mode_text = if mode.is_empty() {
            String::new()
        } else {
            format!("on_error: {mode} · ")
        };
        Some(TapeHandledFailureDetail {
            label,
            what,
            exit_on_error: format!("{exit}{mode_text}the run went on"),
            error,
        })
    });
    let summary = rows
        .iter()
        .rev()
        .find(|r| r.status.kind == StatusKind::Done)
        .and_then(|r| flat.get(&r.path))
        .map(|n| crate::render::display_value(&n.value))
        .filter(|v| !v.is_empty());

    TapeEndedDetail {
        ok,
        duration_s: header.elapsed_s,
        start_ts: header.run_start_ts.clone(),
        end_ts: header.run_end_ts.clone(),
        tokens_sent: header.tokens_sent,
        tokens_received: header.tokens_received,
        usual_total_tokens,
        usual_dev_pct,
        usual_total_s: header.history_usual_total_s,
        effects_done: header.effects_done,
        effects_planned: header.effects_planned,
        handled_count,
        decision_text,
        largest_label: largest.as_ref().map(|(l, _)| l.clone()),
        largest_tokens: largest.map(|(_, t)| t),
        handled_failure: handled_failure_detail,
        summary,
    }
}

/// [`TapeFailedDetail`] (issue #678's §2.1 Failed state).
fn build_tape_failed_detail(
    error: String,
    failing_path: Option<&str>,
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    header: &crate::render::Header,
    state: Option<&Value>,
) -> TapeFailedDetail {
    let failing_row = failing_path.and_then(|p| rows.iter().find(|r| r.path == p));
    let failing_label = failing_row
        .map(|r| tape_row_label(r, flat, plan))
        .unwrap_or_default();
    let provider = failing_row
        .and_then(|r| flat.get(&r.path))
        .and_then(|n| n.meta.get("provider"))
        .and_then(Value::as_str);
    let failing_type = match (failing_row.map(|r| r.kind), provider) {
        (Some(kind), Some(p)) => format!("{} · {p}", kind.label()),
        (Some(kind), None) => kind.label().to_string(),
        _ => String::new(),
    };
    // The last step that actually finished before the failure (issue
    // #678's Failed detail: `kind`'s own tokens and usual, not the
    // whole run's -- comparing a run cut short after one step against
    // the *whole run's* usual total would always read wildly low).
    let last_done = rows.iter().rfind(|r| r.status.kind == StatusKind::Done);
    let (tokens_sent, tokens_received) = last_done
        .and_then(|r| flat.get(&r.path))
        .map(|n| crate::model::node_tokens(&n.meta))
        .unwrap_or((0, 0));
    let usual_tokens = last_done.and_then(|r| tape_row_usual_tokens(&r.path, plan, header));
    let usual_dev_pct = usual_tokens.filter(|&u| u > 0).map(|u| {
        let total = (tokens_sent + tokens_received) as f64;
        ((total - u as f64) / u as f64) * 100.0
    });

    let mut not_reached = Vec::new();
    for row in rows.iter().filter(|r| {
        r.depth == 1 && matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped)
    }) {
        let label = tape_row_label(row, flat, plan);
        let text = match row.kind {
            RowKind::If => {
                let has_else = plan
                    .match_path(&row.path)
                    .and_then(|m| m.entries.first())
                    .is_some_and(|e| matches!(&e.kind, PlanEntryKind::If { else_paths, .. } if !else_paths.is_empty()));
                if has_else {
                    format!("{label} → then or else")
                } else {
                    format!("{label} → then")
                }
            }
            RowKind::Loop => {
                let total = flat
                    .get(&row.path)
                    .and_then(|n| n.progress_total)
                    .or_else(|| state.and_then(|root| plan.resolve_each_len(&row.path, root)));
                match total {
                    Some(total) => format!("{label} · {total} passes"),
                    None => label,
                }
            }
            _ => label,
        };
        not_reached.push(text);
    }

    TapeFailedDetail {
        duration_s: header.elapsed_s,
        start_ts: header.run_start_ts.clone(),
        failing_label,
        failing_type,
        tokens_sent,
        tokens_received,
        usual_tokens,
        usual_dev_pct,
        error,
        not_reached,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_tape(
    rows: &[Row],
    header: &crate::render::Header,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    selected_path: Option<&str>,
    previews_on: bool,
    now: &str,
) -> TapeScreen {
    let run_start = header.run_start_ts.as_deref();
    let elapsed_offset = |ts: &str| run_start.and_then(|s| duration_seconds(s, ts));

    // Each synthetic row lands right after its own loop's last real
    // row in `rows`'s own already-correct DFS order (`walk_rows`'s
    // own, same order bucket1's "ended" sort relies on), not simply
    // appended after every real row regardless of path -- otherwise
    // an un-dispatched pass would read after `flaky`/`summary`
    // instead of where its own loop actually sits.
    let extra_rows = crate::render::missing_loop_pass_rows(rows, flat, plan, state);
    let mut all_rows: Vec<&Row> = rows.iter().collect();
    for (loop_path, extra) in &extra_rows {
        let insert_at = all_rows
            .iter()
            .rposition(|r| r.path.starts_with(&format!("{loop_path}.")))
            .map_or(all_rows.len(), |i| i + 1);
        all_rows.insert(insert_at, extra);
    }

    let mut bucket1: Vec<&Row> = Vec::new();
    let mut bucket2: Vec<&Row> = Vec::new();
    let mut bucket3: Vec<&Row> = Vec::new();
    for row in all_rows {
        // `RowKind::Pass` is a loop pass's own container row (`fold`'s
        // "pass N of 3"), not a step of its own -- the Tape already
        // shows that pass's children with their own `iter_N › ...`
        // breadcrumb (`tape_row_label`), so keeping the container too
        // would draw the same pass twice. `QuickSteps`/`Report`/
        // `Unknown` carry no row text of their own either (§3.4 has no
        // anatomy for them).
        if matches!(
            row.kind,
            RowKind::Root
                | RowKind::Pass
                | RowKind::QuickSteps
                | RowKind::Report
                | RowKind::Unknown
        ) {
            continue;
        }
        let is_active_leaf = row.kind.is_leaf()
            && matches!(
                row.status.kind,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            );
        if is_active_leaf {
            bucket2.push(row);
        } else if matches!(row.status.kind, StatusKind::Pending | StatusKind::Skipped) {
            // A failed run's own untaken rows (§2.1: "unreached rows
            // as skipped") draw exactly like a row ahead -- faint,
            // with "skipped · not reached · prompt" -- never in finish
            // order, which is bucket1's own place for anything that
            // actually ran, finished or not.
            bucket3.push(row);
        } else {
            bucket1.push(row);
        }
    }
    // Once the run is no longer live (Ended/Failed/Cancelled/
    // Aborted), finished rows read in plan order, not real finish
    // order: `rows` itself is already walked depth-first in exactly
    // that order (`walk_rows`/`child_segments`, a pass grouped with
    // its own every leaf before the next pass's), and a real finish
    // order over this same run scrambles a loop's own passes by
    // whichever of two near-simultaneous steps the engine happened
    // to write first. Only while the run is actually running does
    // "the order they finished" (§2.1) mean anything to show.
    let row_order: BTreeMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.path.as_str(), i))
        .collect();
    let finished_order = header.status == RunStatus::Running;
    let sort_key = |row: &&Row| -> f64 {
        if !finished_order {
            return *row_order.get(row.path.as_str()).unwrap_or(&usize::MAX) as f64;
        }
        let node = flat.get(&row.path);
        // An `if`'s own `completed_at` is just the engine's own
        // bookkeeping once its branch is chosen -- not infrequently
        // a touch *after* that branch's own first effect has
        // already finished (both inside the very same millisecond,
        // issue #678's own `gate`/`gate.reply` golden). What a
        // reader actually watched finish first was the decision
        // itself, at `created_at`, always strictly before anything
        // either branch goes on to do.
        let ts = if row.kind == RowKind::If {
            node.and_then(|n| n.created_at.as_deref())
        } else {
            node.and_then(|n| n.completed_at.as_deref())
                .or_else(|| node.and_then(|n| n.created_at.as_deref()))
        };
        ts.and_then(elapsed_offset).unwrap_or(f64::INFINITY)
    };
    bucket1.sort_by(|a, b| sort_key(a).total_cmp(&sort_key(b)));

    // Loops with a running pass right now (bucket2 is never a
    // container, so this groups each active leaf by its own nearest
    // Loop ancestor among bucket1) -- the slots rule (§3.4's "2 of 2
    // Slots Busy · 1 Pass Queued") is drawn once per such loop.
    let running_loops: Vec<&Row> = bucket1
        .iter()
        .filter(|r| r.kind == RowKind::Loop && r.status.kind != StatusKind::Done)
        .copied()
        .collect();

    let head = if run_start.is_none() {
        TapeHead::Starting
    } else {
        match header.status {
            RunStatus::Running => TapeHead::Normal,
            RunStatus::Failed => {
                // The deepest failed leaf, not the root (`prime`
                // itself reads `Failed` too, by the same chain-
                // failure propagation, but is never the row whose own
                // error this head or the detail pane want).
                let failing = rows
                    .iter()
                    .find(|r| r.kind.is_leaf() && r.status.kind == StatusKind::Failed)
                    .or_else(|| rows.iter().find(|r| r.status.kind == StatusKind::Failed));
                let error = failing
                    .and_then(|r| flat.get(&r.path))
                    .and_then(|n| n.error.clone())
                    .unwrap_or_default();
                TapeHead::Failed {
                    error,
                    failing_path: failing.map(|r| r.path.clone()),
                }
            }
            _ => {
                let handled = rows
                    .iter()
                    .find(|r| r.status.kind == StatusKind::FailedHandled)
                    .map(|r| {
                        let error = flat
                            .get(&r.path)
                            .and_then(|n| n.error.clone())
                            .unwrap_or_default();
                        (r.label.clone(), error)
                    });
                TapeHead::Ended {
                    ok: matches!(header.status, RunStatus::Ok),
                    duration_s: header.elapsed_s,
                    handled_failure: handled,
                }
            }
        }
    };
    // §2.1's own frames: a preview line (and the title's own
    // "Previews On/Off") is a Normal/Usual-state-only thing -- every
    // Starting/Ended/Failed row, finished or not, reads plainly.
    let previews_on = previews_on && matches!(head, TapeHead::Normal);

    let mut items: Vec<TapeItem> = Vec::new();
    if matches!(head, TapeHead::Starting) {
        // §2.1: the Starting state has no Run Started row at all yet
        // -- the head rule itself is the list's own first line.
        items.push(TapeItem::HeadRule);
    } else {
        items.push(TapeItem::RunStarted {
            ts: run_start.map(str::to_string),
            document: header.document.clone(),
            // `--events`' own `run_start.engine` carries the version
            // (`cof 0.1.0`), unlike `header.engine` (the bare adapter
            // name every other screen's chip uses) -- §2.1's own Run
            // Started row wants the version.
            engine: model
                .run_start_engine()
                .map(str::to_string)
                .unwrap_or_else(|| header.engine.clone()),
        });
    }

    // The Totals row's own "usual" deviation (§3.3): not the whole
    // run's eventual usual total (comparing tokens so far against
    // the *full* document's own usual would always read wildly low
    // mid-run, or on a run cut short by a failure), but the sum of
    // every already-run-or-running row's own usual tokens -- `0` only
    // ever shown as "no history", never divided into.
    let mut usual_so_far: u64 = 0;
    for row in &bucket1 {
        let (what, preview, mut tokens) =
            tape_row_content(row, flat, plan, model, state, previews_on, false, now);
        tokens.usual = tape_row_usual_tokens(&row.path, plan, header);
        usual_so_far += tokens.usual.unwrap_or(0);
        items.push(TapeItem::Row(TapeRow {
            path: row.path.clone(),
            label: tape_row_label(row, flat, plan),
            kind: row.kind,
            status: row.status.clone(),
            ts: flat
                .get(&row.path)
                .and_then(|n| n.completed_at.clone().or_else(|| n.created_at.clone())),
            what,
            preview,
            bold: false,
            tokens,
        }));
    }

    match head {
        TapeHead::Normal => items.push(TapeItem::NowBand {
            ts: Some(now.to_string()),
            elapsed_s: header.elapsed_s,
        }),
        TapeHead::Failed { .. } | TapeHead::Ended { .. } => items.push(TapeItem::HeadRule),
        TapeHead::Starting => {}
    }

    for row in &bucket2 {
        let (what, preview, mut tokens) =
            tape_row_content(row, flat, plan, model, state, previews_on, false, now);
        tokens.usual = tape_row_usual_tokens(&row.path, plan, header);
        usual_so_far += tokens.usual.unwrap_or(0);
        items.push(TapeItem::Row(TapeRow {
            path: row.path.clone(),
            label: tape_row_label(row, flat, plan),
            kind: row.kind,
            status: row.status.clone(),
            ts: flat.get(&row.path).and_then(|n| n.created_at.clone()),
            what,
            preview,
            bold: true,
            tokens,
        }));
    }
    let usual_so_far_tokens = (usual_so_far > 0).then_some(usual_so_far);

    for loop_row in &running_loops {
        let total = loop_row.loop_progress.and_then(|p| p.total);
        let done = loop_row.loop_progress.map(|p| p.done).unwrap_or(0);
        let prefix = format!("{}.", loop_row.path);
        let busy = bucket2
            .iter()
            .filter(|r| r.path.starts_with(&prefix))
            .count() as u64;
        let slots = model
            .running_or_queued_bound(&loop_row.path)
            .unwrap_or(busy);
        let queued = total.map(|t| t.saturating_sub(done).saturating_sub(busy));
        if let Some(queued) = queued {
            items.push(TapeItem::SlotsRule {
                busy,
                slots,
                queued,
            });
        }
    }

    for row in &bucket3 {
        let queued = tape_row_is_under_a_queued_pass(&row.path)
            && running_loops
                .iter()
                .any(|l| row.path.starts_with(&format!("{}.", l.path)));
        let (what, preview, mut tokens) =
            tape_row_content(row, flat, plan, model, state, previews_on, queued, now);
        // §2.1: a row ahead shows `usually N t` since it's still
        // expected to run; a failed run's own unreached rows, truly
        // skipped rather than merely not-yet-reached, never do.
        if row.status.kind != StatusKind::Skipped {
            tokens.usual = tape_row_usual_tokens(&row.path, plan, header);
        }
        items.push(TapeItem::Row(TapeRow {
            path: row.path.clone(),
            label: tape_row_label(row, flat, plan),
            kind: row.kind,
            status: row.status.clone(),
            ts: None,
            what,
            preview,
            bold: false,
            tokens,
        }));
    }

    if matches!(head, TapeHead::Starting | TapeHead::Normal) {
        items.push(TapeItem::RunEnd {
            usual_total_tokens: header.history_usual_total_tokens,
            usual_wall_s: header.history_usual_total_s,
            eta_ts: header.history_eta_ts.clone(),
        });
    }

    let largest = tape_largest_tokens(&items);
    for item in &mut items {
        if let TapeItem::Row(row) = item {
            if let Some(total) = row.tokens.total() {
                if Some(total) == largest && largest.is_some() {
                    row.tokens.is_largest = true;
                }
            }
        }
    }

    // §9's own §2, settled: the unit is derived from the usual
    // largest step across every plan-known step history has a
    // figure for, `50` with none -- the design's own `hist/run-h1..3`
    // give `prime.summary` a usual 402 t, which still draws (and so
    // fits) an 8-cell bar at 50 t, so every frame's own fixed-looking
    // `▰ = 50 t` title already matches this derivation, not a
    // separate fixed default.
    let largest_usual_step = header
        .history_step_usual
        .values()
        .filter_map(|s| s.tokens_p50)
        .max();
    let unit = tape_unit(largest_usual_step);

    // §2.1's Tape title counts a finished decision too (`gate →
    // then`), not leaves alone -- only a loop container's own row is
    // excluded, since its passes are each already counted on their
    // own.
    let done = bucket1
        .iter()
        .filter(|r| {
            r.kind != RowKind::Loop
                && matches!(r.status.kind, StatusKind::Done | StatusKind::FailedHandled)
        })
        .count() as u64;
    let running = bucket2.len() as u64;
    // Every row ahead counts, container and leaf alike (a failed
    // run's own untaken decision and loop read "skipped" too, same
    // as their own leaves) -- unlike `done`, which excludes a loop's
    // own container row since its passes are already counted on
    // their own.
    let ahead = bucket3.len() as u64;

    let selected = selected_path
        .and_then(|p| rows.iter().find(|r| r.path == p))
        .map(|row| {
            build_focused_step(
                row,
                flat,
                plan,
                model,
                state,
                now,
                header.run_end_ts.as_deref(),
            )
        });

    let selected_usual = selected_path
        .and_then(|p| crate::history::display_key(p, plan))
        .and_then(|key| header.history_step_usual.get(&key))
        .and_then(|s| s.tokens_p50.map(|t| (t, s.count)));

    let selected_share_pct = selected_path.and_then(|p| {
        let node = flat.get(p)?;
        let (sent, received) = crate::model::node_tokens(&node.meta);
        let total = sent + received;
        let run_total = header.tokens_sent + header.tokens_received;
        (run_total > 0).then(|| ((total as f64 * 100.0) / run_total as f64).round() as u64)
    });
    let selected_row = selected_path.and_then(|p| rows.iter().find(|r| r.path == p));
    let selected_breadcrumb = selected_row.map(|row| tape_row_label(row, flat, plan));
    let selected_detail = selected_row.map(|row| build_tape_selected_detail(row, flat, model, now));

    let starting_detail = matches!(head, TapeHead::Starting)
        .then(|| build_tape_starting_detail(rows, flat, plan, header, state));
    let ended_detail = match &head {
        TapeHead::Ended {
            ok,
            handled_failure,
            ..
        } => Some(build_tape_ended_detail(
            *ok,
            handled_failure.clone(),
            &items,
            rows,
            flat,
            plan,
            header,
        )),
        _ => None,
    };
    let failed_detail = match &head {
        TapeHead::Failed {
            error,
            failing_path,
        } => Some(build_tape_failed_detail(
            error.clone(),
            failing_path.as_deref(),
            rows,
            flat,
            plan,
            header,
            state,
        )),
        _ => None,
    };

    TapeScreen {
        unit,
        items,
        done,
        running,
        ahead,
        head,
        selected,
        selected_breadcrumb,
        selected_usual,
        selected_share_pct,
        selected_detail,
        starting_detail,
        ended_detail,
        failed_detail,
        usual_so_far_tokens,
    }
}

// ---------------------------------------------------------------------
// Screen 1: Signal (issue #688)
// ---------------------------------------------------------------------

/// One column's own totals (§2.2's trace, §3.5's bucketing rule): a
/// column spans [`col * column_s`, `(col+1) * column_s`) seconds
/// since `run_start`, and every finished prompt's own sent/received
/// pair lands in the column that holds its own end time -- `sent`/
/// `received` are this column's whole sum, over every prompt whose
/// end landed here, not one prompt's own figure. `sent_live`/
/// `received_live` mark a running prompt's own estimate, counted
/// only in the Now column, growing it as the estimate grows, the
/// same mutually-exclusive rule the Tape's own live estimate follows
/// -- never both a finished node's exact figure and its own running
/// estimate in the same redraw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SignalBucket {
    pub sent: u64,
    pub received: u64,
    pub sent_live: bool,
    pub received_live: bool,
    /// How many distinct prompts contributed to this column (the Now
    /// pane's own "3 prompts in one step").
    pub prompt_count: u64,
    /// The latest timestamp among this column's own contributors --
    /// Peak's own clock and "time since the last burst" both read
    /// this directly, rather than re-deriving a clock from the
    /// column's own index (which the run's own zone and rounding
    /// could disagree with).
    pub last_ts: Option<String>,
}

/// What a trace column's own axis mark means (§3.7): a decision wins
/// over a finished prompt when both land in the same column -- the
/// design's own `run2` fixture has three prompts and one decision
/// share a column, and the frame marks it `◆`, never `✓`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalMark {
    Done,
    Decision,
}

/// A step the decision's own branch already released (§2.2: "a
/// decision with its branch and the steps it released"), styled as
/// its own run of spans by a front end -- `label`/`tokens`/`dev_pct`
/// read the same way a Tape row's own would.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalReleasedStep {
    pub label: String,
    pub tokens: Option<u64>,
    pub dev_pct: Option<f64>,
}

/// A step row's own kind-specific content (§2.2's "step rows under
/// the axis"), already composed text wherever a single style covers
/// the whole fragment -- a front end never re-derives a kind's own
/// shape from a path.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalStepKind {
    /// `215 t · +6% · Bug.` (the deviation and the reply's own first
    /// words each absent when there is nothing to show).
    Prompt { rest: String },
    /// `sleep 3 · 3.0s`, drawn as a bar from this row's own `col` to
    /// `end_col`, the label overlaid at its own start and the rest
    /// of the span filled with the row's own bar rule.
    Tool { end_col: usize, text: String },
    /// The branch's own released steps (the row's own `label`,
    /// already `gate → then`, names the branch itself), and a
    /// trailing muted `suffix`: the very next top-level row's own
    /// progress, when it is an already-active loop -- the design
    /// text says nothing about it, only the `run2` frame does.
    Decision {
        released: Vec<SignalReleasedStep>,
        suffix: String,
    },
    /// `2 Running · 1 Queued` (the same slots rule the Tape's own
    /// `SlotsRule` already computes).
    Loop { status_text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalStepRow {
    pub path: String,
    pub label: String,
    pub col: usize,
    pub row_kind: RowKind,
    pub status: StatusKind,
    pub kind: SignalStepKind,
}

/// A Now pane row for a running leaf (§2.2: `● drafts › iter_0 ›
/// pause   0.6s · sleep 1 · slot 1`) -- `label` is the full
/// breadcrumb from the run's own root, since the Now pane (unlike
/// the Tape's own list) has no loop heading above it to carry the
/// loop's own name instead.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalRunningRow {
    pub path: String,
    pub label: String,
    pub what: String,
    pub slot: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SignalNowPane {
    /// `2 of 2 Slots Busy` -- `None` with no loop currently busy
    /// (the pane's own title falls back to a bare `Now`).
    pub slots_text: Option<String>,
    pub running: Vec<SignalRunningRow>,
    /// A pass waiting for a free slot (§2.2: `· drafts › iter_2
    /// waits for a slot`).
    pub waiting: Vec<(String, String)>,
    /// The active loop's own row (§2.2: `⟳ drafts  0 of 3 passes ·
    /// 130 t per pass`) -- label and the status text after it.
    pub loop_summary: Option<(String, String)>,
    pub last_burst: Option<String>,
    pub next_signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SignalReadings {
    pub sent: u64,
    pub received: u64,
    /// Sent to received, to one decimal (§2.2: `11.5 : 1`) -- `None`
    /// with nothing received yet.
    pub ratio: Option<f64>,
    /// The largest column's own sent figure and its clock.
    pub peak: Option<(u64, String)>,
    /// The single largest finished prompt's own breadcrumb and total.
    pub largest: Option<(String, u64)>,
    pub quiet_s: Option<f64>,
}

/// Screen 1's own model (issue #688, design v1 part 2, replacing
/// Time): the trace's own columns and axis marks, the step rows
/// under it, the Now pane and Readings -- built from exactly the
/// same inputs [`build_tape`] already has, plus `column_ms` (§3.5/
/// §4's own zoom, 50-1,000 ms per column, 100 the default) -- no
/// engine change, same as the Tape's own model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SignalScreen {
    /// The run's own model name, for the title's `Tokens to
    /// scripted-model` -- `None` when nothing has run yet.
    pub model_label: Option<String>,
    pub column_ms: u64,
    pub sent_axis_top: u64,
    pub received_axis_top: u64,
    pub buckets: Vec<SignalBucket>,
    pub marks: Vec<(usize, SignalMark)>,
    /// The Now sweep's own column, `None` before the run has a start
    /// time at all.
    pub now_col: Option<usize>,
    /// `now`'s own raw timestamp, for a front end's own clock
    /// formatting (never pre-formatted here, same reason a
    /// `TapeRow`'s own `ts` isn't) -- `None` exactly when `now_col`
    /// is.
    pub now_ts: Option<String>,
    /// `now`'s own elapsed seconds since `run_start` -- the Now
    /// label's own `3.6s`, kept alongside the raw clock so a front
    /// end never has to re-derive it from the two timestamps.
    pub now_elapsed_s: Option<f64>,
    /// `←→`'s own scrub cursor, a column index -- equal to
    /// `now_col` while following, a smaller column once scrubbed.
    /// `tui::draw_signal_chart` draws this distinctly from the Now
    /// sweep whenever the two differ.
    pub cursor_col: Option<usize>,
    /// The whole run's own usual end, raw (`header.history_eta_ts`),
    /// for the `Ahead` label's own `usually ends <clock>`.
    pub ahead_eta_ts: Option<String>,
    pub step_rows: Vec<SignalStepRow>,
    /// The faint plan-ahead list trailing the *last* step row: the
    /// current loop's own next pass, then every top-level row still
    /// ahead, then `■ End` -- `None` once nothing is ahead any more
    /// (a loop with no queued pass and no sibling left, on a
    /// finished run).
    pub ahead_suffix: Option<String>,
    pub now_pane: SignalNowPane,
    pub readings: SignalReadings,
    /// The Help overlay's own token legend reads this the same way
    /// the Tape's does -- Signal draws no bars of its own at this
    /// unit, but keeps the same field name so `tui::draw`'s shared
    /// dialog plumbing needs no Signal-specific case of its own.
    pub unit: u64,
    /// The step under the scrub cursor (Now when not scrubbed), for
    /// `enter`'s own full-screen step view (§4: "opens the step
    /// under the cursor") -- the same [`FocusedStep`] the Tape's own
    /// detail pane builds theirs from.
    pub selected: Option<FocusedStep>,
    /// The Totals row's own "usual" deviation (§3.3), the same sum
    /// over every already-run-or-running row's own usual tokens the
    /// Tape's own `usual_so_far_tokens` is.
    pub usual_so_far_tokens: Option<u64>,
    /// §3.2's status line: the most recently finished row, any
    /// depth -- a front end reads its own glyph/colour straight off
    /// `kind`/`status`, the same way the Tape's own status line
    /// already does for its own last row.
    pub last_finished: Option<SignalLastFinished>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalLastFinished {
    pub label: String,
    pub kind: RowKind,
    pub status: StatusKind,
    pub tokens: Option<(u64, u64)>,
    pub ts: Option<String>,
}

/// §3.5's own doubling series (`300, 600, 1,200, 2,400, ...` for
/// sent, `40, 80, 160, 320, ...` for received): the smallest value
/// in the series starting at `start` that is at least `need`.
fn signal_axis_series_min(start: u64, need: u64) -> u64 {
    let mut v = start;
    while v < need {
        v *= 2;
    }
    v
}

/// §3.5's axis rule: with history, the smallest of the doubling
/// series that holds 1.5x the usual largest step's own sent (or
/// received) figure; without history, 600 (or 80), doubled exactly
/// once if the run's own largest single column has already passed
/// it, and never again -- the axis never slides back down, and
/// never doubles a second time.
fn signal_axis_top(largest_usual: Option<u64>, largest_col: u64, start: u64) -> u64 {
    match largest_usual.filter(|&v| v > 0) {
        Some(largest) => {
            let need = (largest as f64 * 1.5).ceil() as u64;
            signal_axis_series_min(start, need)
        }
        None => {
            if largest_col > start {
                start * 2
            } else {
                start
            }
        }
    }
}

/// A leaf's own breadcrumb relative to the run's root (§2.2's Now
/// pane: `drafts › iter_0 › pause`, never stripped of its own loop
/// name the way the Tape's list rows are).
fn signal_breadcrumb(path: &str) -> String {
    crate::cost::path_breadcrumb_relative_to(path, "prime")
}

/// `path` cut back to its own first `iter_N` segment (`prime.drafts.
/// iter_2.draft` to `prime.drafts.iter_2`) -- `None` for a path with
/// no loop pass segment at all.
fn signal_pass_path(path: &str) -> Option<&str> {
    let mut end = 0;
    for (i, seg) in path.split('.').enumerate() {
        end += seg.len() + if i == 0 { 0 } else { 1 };
        if seg.starts_with("iter_") {
            return Some(&path[..end]);
        }
    }
    None
}

/// The sum of tokens over every pass of `loop_path` that has one so
/// far, and how many such passes there are -- the loop row's own
/// `130 t per pass` and a decision's own trailing `· drafts ×2 ·
/// 261 t` both read this.
fn signal_loop_pass_tokens(loop_path: &str, flat: &BTreeMap<String, NodeMeta>) -> (u64, u64) {
    let prefix = format!("{loop_path}.iter_");
    let mut per_pass: BTreeMap<u64, u64> = BTreeMap::new();
    for (path, node) in flat {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        let Some(Ok(idx)) = rest.split('.').next().map(str::parse) else {
            continue;
        };
        let (sent, received) = crate::model::node_tokens(&node.meta);
        *per_pass.entry(idx).or_insert(0) += sent + received;
    }
    let with_tokens: Vec<u64> = per_pass.values().copied().filter(|&t| t > 0).collect();
    (with_tokens.iter().sum(), with_tokens.len() as u64)
}

#[allow(clippy::too_many_arguments)]
/// The newest finished (or handled-failed) row, by its own finish
/// time (DESIGN.md §3.2: "the newest finished step") -- shared by
/// the Signal's and the Flow's own status lines.
pub(crate) fn last_finished_row(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    run_start: Option<&str>,
) -> Option<SignalLastFinished> {
    let elapsed_offset = |ts: &str| run_start.and_then(|s| duration_seconds(s, ts));
    let finish_ts = |r: &&Row| -> Option<&str> {
        let node = flat.get(&r.path)?;
        if r.kind == RowKind::If {
            node.created_at.as_deref()
        } else {
            node.completed_at.as_deref()
        }
    };
    rows.iter()
        .filter(|r| matches!(r.status.kind, StatusKind::Done | StatusKind::FailedHandled))
        .filter_map(|r| Some((r, elapsed_offset(finish_ts(&r)?)?)))
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(row, _)| {
            let node = flat.get(&row.path);
            let tokens = node.map(|n| crate::model::node_tokens(&n.meta));
            SignalLastFinished {
                label: tape_row_label(row, flat, plan),
                kind: row.kind,
                status: row.status.kind,
                tokens: tokens.filter(|(s, r)| s + r > 0),
                ts: finish_ts(&row).map(str::to_string),
            }
        })
}

#[allow(clippy::too_many_arguments)]
pub fn build_signal(
    rows: &[Row],
    header: &crate::render::Header,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    column_ms: u64,
    cursor_col: Option<usize>,
    now: &str,
) -> SignalScreen {
    // Once a run has ended, its own Now sweep stays at the run's
    // own end rather than drifting on with the wall clock a caller
    // still passes in (`osi watch` of an old run, say) -- the run
    // has no later activity for a sweep past its end to mean.
    let now = header.run_end_ts.as_deref().unwrap_or(now);
    let run_start = header.run_start_ts.as_deref();
    let elapsed_offset = |ts: &str| run_start.and_then(|s| duration_seconds(s, ts));
    let column_s = (column_ms.max(1) as f64) / 1000.0;
    let col_of = |offset_s: f64| (offset_s / column_s).floor().max(0.0) as usize;
    // The axis's own no-history fallback (§3.5) measures the
    // largest single column against 600/80, never the run's own
    // running total -- at a fixed 100 ms base so zooming the
    // display columns can't change what the axis reads.
    let col_of_100 = |offset_s: f64| (offset_s / 0.1).floor().max(0.0) as usize;
    let now_offset = elapsed_offset(now);
    let now_col = now_offset.map(col_of);
    // ←→'s own scrub cursor (§4): a column at or before Now --
    // Readings' own Peak, Largest and Quiet all read up to here, not
    // past it, once scrubbed away from Now.
    let cursor = cursor_col.or(now_col);

    let model_label = rows
        .iter()
        .filter(|r| r.kind == RowKind::Prompt)
        .find_map(|r| {
            flat.get(&r.path)
                .and_then(|n| n.meta.get("model"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);

    let mut buckets: Vec<SignalBucket> = Vec::new();
    let mut marks: BTreeMap<usize, SignalMark> = BTreeMap::new();
    let mut fixed_cols: BTreeMap<usize, (u64, u64)> = BTreeMap::new();
    fn ensure_len(buckets: &mut Vec<SignalBucket>, len: usize) {
        if buckets.len() < len {
            buckets.resize(len, SignalBucket::default());
        }
    }
    for row in rows {
        let node = flat.get(&row.path);
        match row.kind {
            RowKind::Prompt | RowKind::Reflector => {
                let is_running = matches!(
                    row.status.kind,
                    StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                );
                if is_running {
                    let Some(now_col) = now_col else { continue };
                    let Some(live) = crate::model::prompt_live_figures(node, &row.path, model, now)
                    else {
                        continue;
                    };
                    ensure_len(&mut buckets, now_col + 1);
                    let b = &mut buckets[now_col];
                    if let Some((sent, live_sent)) = live.tokens_sent_or_estimate() {
                        b.sent += sent;
                        b.sent_live = b.sent_live || live_sent;
                    }
                    if let Some((received, live_recv)) = live.tokens_received_or_estimate() {
                        b.received += received;
                        b.received_live = b.received_live || live_recv;
                    }
                    b.prompt_count += 1;
                    b.last_ts = Some(now.to_string());
                    if let Some(now_offset) = now_offset {
                        let fixed = fixed_cols.entry(col_of_100(now_offset)).or_default();
                        fixed.0 += live.tokens_sent_or_estimate().map(|(v, _)| v).unwrap_or(0);
                        fixed.1 += live
                            .tokens_received_or_estimate()
                            .map(|(v, _)| v)
                            .unwrap_or(0);
                    }
                } else if row.status.kind == StatusKind::Done
                    || row.status.kind == StatusKind::FailedHandled
                {
                    let Some(node) = node else { continue };
                    let Some(end_ts) = node.completed_at.as_deref() else {
                        continue;
                    };
                    let Some(offset) = elapsed_offset(end_ts) else {
                        continue;
                    };
                    let col = col_of(offset);
                    ensure_len(&mut buckets, col + 1);
                    let (sent, received) = crate::model::node_tokens(&node.meta);
                    let b = &mut buckets[col];
                    b.sent += sent;
                    b.received += received;
                    b.prompt_count += 1;
                    b.last_ts = Some(end_ts.to_string());
                    marks.entry(col).or_insert(SignalMark::Done);
                    let fixed = fixed_cols.entry(col_of_100(offset)).or_default();
                    fixed.0 += sent;
                    fixed.1 += received;
                }
            }
            RowKind::If => {
                let Some(node) = node else { continue };
                if node.branch.is_none() {
                    continue;
                }
                let Some(start_ts) = node.created_at.as_deref() else {
                    continue;
                };
                let Some(offset) = elapsed_offset(start_ts) else {
                    continue;
                };
                marks.insert(col_of(offset), SignalMark::Decision);
            }
            _ => {}
        }
    }

    let largest_col_sent = fixed_cols.values().map(|(s, _)| *s).max().unwrap_or(0);
    let largest_col_received = fixed_cols.values().map(|(_, r)| *r).max().unwrap_or(0);
    let sent_axis_top_val = signal_axis_top(
        header
            .history_step_usual
            .values()
            .filter_map(|s| s.tokens_sent_p50)
            .max(),
        largest_col_sent,
        600,
    );
    let received_axis_top_val = signal_axis_top(
        header
            .history_step_usual
            .values()
            .filter_map(|s| s.tokens_received_p50)
            .max(),
        largest_col_received,
        80,
    );

    let now_ts = now_col.map(|_| now.to_string());
    let ahead_eta_ts = header.history_eta_ts.clone();

    // §2.2's own step rows: one per top-level entry that has started
    // (`rows`'s own depth-1 slice, in plan order -- never real finish
    // order, which this screen has no Now band to anchor against the
    // way the Tape's own list does), each at its own start column.
    let top_level: Vec<&Row> = rows.iter().filter(|r| r.depth == 1).collect();
    let started: Vec<&&Row> = top_level
        .iter()
        .filter(|r| !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped))
        .collect();
    let mut step_rows = Vec::new();
    for row in &started {
        let Some(node) = flat.get(&row.path) else {
            continue;
        };
        let Some(start_ts) = node.created_at.as_deref() else {
            continue;
        };
        let Some(offset) = elapsed_offset(start_ts) else {
            continue;
        };
        let col = col_of(offset);
        let label = tape_row_label(row, flat, plan);
        let kind = match row.kind {
            RowKind::Prompt | RowKind::Reflector => {
                let (sent, received) = crate::model::node_tokens(&node.meta);
                let total = sent + received;
                let usual = tape_row_usual_tokens(&row.path, plan, header);
                let dev = usual
                    .filter(|&u| u > 0)
                    .map(|u| ((total as f64 - u as f64) / u as f64) * 100.0);
                let reply_words = crate::render::display_value(&node.value);
                let reply_words = (!reply_words.is_empty()).then(|| first_words(&reply_words, 24));
                let mut parts = vec![format!(
                    "{} t",
                    crate::format::format_tokens_thousands(total)
                )];
                if let Some(dev) = dev {
                    parts.push(format!("{dev:+.0}%"));
                }
                if let Some(words) = reply_words {
                    parts.push(words);
                }
                SignalStepKind::Prompt {
                    rest: parts.join(" \u{b7} "),
                }
            }
            RowKind::If => {
                let prefix = format!("{}.", row.path);
                let released = rows
                    .iter()
                    .filter(|r| {
                        r.depth == row.depth + 1
                            && r.path.starts_with(&prefix)
                            && !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped)
                    })
                    .map(|r| {
                        let rn = flat.get(&r.path);
                        let (sent, received) = rn
                            .map(|n| crate::model::node_tokens(&n.meta))
                            .unwrap_or((0, 0));
                        let total = sent + received;
                        let usual = tape_row_usual_tokens(&r.path, plan, header);
                        let dev = usual
                            .filter(|&u| u > 0)
                            .map(|u| ((total as f64 - u as f64) / u as f64) * 100.0);
                        SignalReleasedStep {
                            label: r.label.clone(),
                            tokens: (total > 0).then_some(total),
                            dev_pct: dev,
                        }
                    })
                    .collect();
                // The very next top-level row's own progress, when
                // it is an already-active loop -- the design's text
                // never names this, only `run2`'s own frame does.
                let suffix = top_level
                    .iter()
                    .position(|r| r.path == row.path)
                    .and_then(|idx| top_level.get(idx + 1))
                    .filter(|r| {
                        r.kind == RowKind::Loop
                            && !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped)
                    })
                    .map(|r| {
                        let (tokens, count) = signal_loop_pass_tokens(&r.path, flat);
                        format!(
                            "\u{b7} {} \u{d7}{count} \u{b7} {} t",
                            tape_row_label(r, flat, plan),
                            crate::format::format_tokens_thousands(tokens)
                        )
                    })
                    .unwrap_or_default();
                SignalStepKind::Decision { released, suffix }
            }
            RowKind::Loop => {
                let busy = rows
                    .iter()
                    .filter(|r| {
                        r.path.starts_with(&format!("{}.", row.path))
                            && r.kind.is_leaf()
                            && matches!(
                                r.status.kind,
                                StatusKind::Running
                                    | StatusKind::LikelyRunning
                                    | StatusKind::RunningOrQueued
                            )
                    })
                    .count() as u64;
                let total = row.loop_progress.and_then(|p| p.total);
                let done = row.loop_progress.map(|p| p.done).unwrap_or(0);
                let queued = total.map(|t| t.saturating_sub(done).saturating_sub(busy));
                let status_text = match queued {
                    Some(queued) => format!("{busy} Running \u{b7} {queued} Queued"),
                    None => format!("{busy} Running"),
                };
                SignalStepKind::Loop { status_text }
            }
            _ => {
                let what = tape_tool_what(row, Some(node));
                let (cmd, duration) = match what.split_once(" \u{b7} ") {
                    Some((d, c)) => (c.to_string(), d.to_string()),
                    None => (String::new(), what),
                };
                let text = if cmd.is_empty() {
                    format!("{label} \u{b7} {duration}")
                } else {
                    format!("{label} \u{b7} {cmd} \u{b7} {duration}")
                };
                let end_col = row
                    .duration_s
                    .map(|d| col + ((d / column_s).round() as usize).max(1) - 1)
                    .unwrap_or(col);
                SignalStepKind::Tool { end_col, text }
            }
        };
        step_rows.push(SignalStepRow {
            path: row.path.clone(),
            label,
            col,
            row_kind: row.kind,
            status: row.status.kind,
            kind,
        });
    }

    // The faint plan-ahead list trailing the last step row: the
    // last started top-level row's own loop, if any, names its own
    // next undispatched pass first; then every top-level row still
    // ahead, in plan order; then `■ End` while the run is live.
    let last_started = started.last().copied();
    let mut ahead_items: Vec<String> = Vec::new();
    if let Some(row) = last_started {
        if row.kind == RowKind::Loop {
            let busy = rows
                .iter()
                .filter(|r| {
                    r.path.starts_with(&format!("{}.", row.path))
                        && r.kind.is_leaf()
                        && matches!(
                            r.status.kind,
                            StatusKind::Running
                                | StatusKind::LikelyRunning
                                | StatusKind::RunningOrQueued
                        )
                })
                .count() as u64;
            let done = row.loop_progress.map(|p| p.done).unwrap_or(0);
            let total = row.loop_progress.and_then(|p| p.total);
            if total.is_none_or(|t| done + busy < t) {
                ahead_items.push(format!("iter_{}", done + busy));
            }
        }
    }
    for row in top_level
        .iter()
        .filter(|r| matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped))
    {
        ahead_items.push(tape_row_label(row, flat, plan));
    }
    let ahead_suffix = if ahead_items.is_empty() {
        header
            .run_end_ts
            .is_none()
            .then(|| "\u{25a0} End".to_string())
    } else {
        let mut text = ahead_items
            .iter()
            .map(|i| format!("\u{b7} {i}"))
            .collect::<Vec<_>>()
            .join("   ");
        if header.run_end_ts.is_none() {
            text.push_str("   \u{25a0} End");
        }
        Some(text)
    };

    // The Now pane.
    let running_loops: Vec<&Row> = top_level
        .iter()
        .filter(|r| r.kind == RowKind::Loop && r.status.kind != StatusKind::Done)
        .copied()
        .collect();
    let slots_text = running_loops.first().map(|loop_row| {
        let prefix = format!("{}.", loop_row.path);
        let busy = rows
            .iter()
            .filter(|r| {
                r.path.starts_with(&prefix)
                    && r.kind.is_leaf()
                    && matches!(
                        r.status.kind,
                        StatusKind::Running
                            | StatusKind::LikelyRunning
                            | StatusKind::RunningOrQueued
                    )
            })
            .count() as u64;
        let slots = model
            .running_or_queued_bound(&loop_row.path)
            .unwrap_or(busy);
        format!("{busy} of {slots} Slots Busy")
    });
    let mut running = Vec::new();
    let mut per_loop_slot: BTreeMap<String, u64> = BTreeMap::new();
    for row in rows.iter().filter(|r| {
        r.kind.is_leaf()
            && matches!(
                r.status.kind,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            )
    }) {
        let (what, _, _) = tape_row_content(row, flat, plan, model, state, false, false, now);
        let loop_path = running_loops
            .iter()
            .find(|l| row.path.starts_with(&format!("{}.", l.path)))
            .map(|l| l.path.clone());
        let slot = loop_path.map(|lp| {
            let n = per_loop_slot.entry(lp).or_insert(0);
            *n += 1;
            *n
        });
        running.push(SignalRunningRow {
            path: row.path.clone(),
            label: signal_breadcrumb(&row.path),
            what,
            slot,
        });
    }
    // A pass not yet dispatched (`iter_2`, say) has no row of its
    // own in `rows` at all, only once it starts -- the same gap
    // `build_tape`'s own `missing_loop_pass_rows` fills in for the
    // Tape's list, needed here for "waits for a slot" and "Next
    // Signal" to see an undispatched pass's own leaves too.
    let extra_rows = crate::render::missing_loop_pass_rows(rows, flat, plan, state);
    let augmented: Vec<&Row> = rows
        .iter()
        .chain(extra_rows.iter().map(|(_, r)| r))
        .collect();

    let mut waiting = Vec::new();
    for loop_row in &running_loops {
        let prefix = format!("{}.", loop_row.path);
        // The pass itself never gets a row of its own (only its own
        // leaves do) -- the first pending leaf under a queued pass
        // names it, its own breadcrumb cut back to the pass
        // (`drafts › iter_2`, never drilling into the leaf).
        if let Some(next) = augmented.iter().find(|r| {
            r.path.starts_with(&prefix)
                && tape_row_is_under_a_queued_pass(&r.path)
                && r.status.kind == StatusKind::Pending
        }) {
            if let Some(pass_path) = signal_pass_path(&next.path) {
                waiting.push((signal_breadcrumb(pass_path), "waits for a slot".to_string()));
            }
        }
    }
    let loop_summary = running_loops.first().map(|loop_row| {
        let done = loop_row.loop_progress.map(|p| p.done).unwrap_or(0);
        let total = loop_row
            .loop_progress
            .and_then(|p| p.total)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());
        let (tokens, count) = signal_loop_pass_tokens(&loop_row.path, flat);
        let per_pass = match tokens.checked_div(count) {
            Some(avg) => format!(" \u{b7} {avg} t per pass"),
            None => String::new(),
        };
        (
            tape_row_label(loop_row, flat, plan),
            format!("{done} of {total} passes{per_pass}"),
        )
    });

    let last_burst_idx = buckets
        .iter()
        .enumerate()
        .rev()
        .find(|(_, b)| b.sent + b.received > 0)
        .map(|(i, _)| i);
    let last_burst = last_burst_idx.and_then(|i| {
        let b = &buckets[i];
        let ts = b.last_ts.as_deref()?;
        let age = duration_seconds(ts, now)?;
        Some(format!(
            "{} ago \u{b7} \u{2191}{} \u{2193}{} \u{b7} {} prompt{} in one step",
            crate::format::format_duration(age),
            crate::format::format_tokens_thousands(b.sent),
            crate::format::format_tokens_thousands(b.received),
            b.prompt_count,
            if b.prompt_count == 1 { "" } else { "s" },
        ))
    });
    // An undispatched pass's own leaf (an `extra_rows` entry)
    // always precedes a later top-level sibling in real plan order
    // (its own loop is already running), so it's checked first --
    // `rows` alone would read a top-level row like `summary` as
    // "next" instead, skipping over the loop's own queued pass
    // entirely (`augmented`'s own chained order is append-only, not
    // plan order).
    let is_pending_prompt = |r: &&Row| {
        matches!(r.kind, RowKind::Prompt | RowKind::Reflector)
            && r.status.kind == StatusKind::Pending
    };
    let next_signal_row = extra_rows
        .iter()
        .map(|(_, r)| r)
        .find(|r| is_pending_prompt(r))
        .or_else(|| rows.iter().find(is_pending_prompt));
    let next_signal = next_signal_row.map(|r| {
        let queued = tape_row_is_under_a_queued_pass(&r.path)
            && running_loops
                .iter()
                .any(|l| r.path.starts_with(&format!("{}.", l.path)));
        let qualifier = if queued { "next slot" } else { "queued" };
        let usual = tape_row_usual_tokens(&r.path, plan, header);
        let usual_text = usual
            .map(|u| {
                format!(
                    " \u{b7} usually {} t",
                    crate::format::format_tokens_thousands(u)
                )
            })
            .unwrap_or_default();
        format!(
            "{} \u{b7} {qualifier}{usual_text}",
            tape_row_label(r, flat, plan)
        )
    });

    // Readings: Peak, Largest and Quiet all read only up to the
    // cursor's own column (Now's, when not scrubbed), the same
    // moment the trace itself stops drawing at.
    let cursor_buckets = &buckets[..cursor.map(|c| (c + 1).min(buckets.len())).unwrap_or(0)];
    let ratio = (header.tokens_received > 0)
        .then(|| (header.tokens_sent as f64 / header.tokens_received as f64 * 10.0).round() / 10.0);
    let peak = cursor_buckets
        .iter()
        .filter(|b| b.sent > 0)
        .max_by_key(|b| b.sent)
        .and_then(|b| Some((b.sent, b.last_ts.clone()?)));
    let mut largest: Option<(String, u64)> = None;
    for row in rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Prompt | RowKind::Reflector))
    {
        let Some(node) = flat.get(&row.path) else {
            continue;
        };
        let in_view = node
            .completed_at
            .as_deref()
            .and_then(elapsed_offset)
            .map(col_of)
            .is_some_and(|c| cursor.is_none_or(|cur| c <= cur));
        if !in_view {
            continue;
        }
        let (sent, received) = crate::model::node_tokens(&node.meta);
        let total = sent + received;
        if total == 0 {
            continue;
        }
        if largest.as_ref().is_none_or(|(_, t)| total > *t) {
            largest = Some((tape_row_label(row, flat, plan), total));
        }
    }
    let cursor_burst_idx = cursor_buckets
        .iter()
        .enumerate()
        .rev()
        .find(|(_, b)| b.sent + b.received > 0)
        .map(|(i, _)| i);
    let quiet_s = cursor_burst_idx
        .and_then(|i| cursor_buckets[i].last_ts.as_deref())
        .and_then(|ts| duration_seconds(ts, now));

    let largest_usual_step = header
        .history_step_usual
        .values()
        .filter_map(|s| s.tokens_p50)
        .max();
    let unit = tape_unit(largest_usual_step);

    // §4's own "opens the step under the cursor": the step row with
    // the greatest start column at or before the cursor (Now's own
    // column when not scrubbed) is whichever step the cursor is
    // currently sitting over.
    let selected = cursor
        .and_then(|c| {
            step_rows
                .iter()
                .filter(|r| r.col <= c)
                .max_by_key(|r| r.col)
        })
        .and_then(|r| rows.iter().find(|row| row.path == r.path))
        .map(|row| {
            build_focused_step(
                row,
                flat,
                plan,
                model,
                state,
                now,
                header.run_end_ts.as_deref(),
            )
        });

    SignalScreen {
        model_label,
        column_ms,
        sent_axis_top: sent_axis_top_val,
        received_axis_top: received_axis_top_val,
        buckets,
        marks: marks.into_iter().collect(),
        now_col,
        now_ts,
        now_elapsed_s: now_offset,
        cursor_col: cursor,
        ahead_eta_ts,
        step_rows,
        ahead_suffix,
        now_pane: SignalNowPane {
            slots_text,
            running,
            waiting,
            loop_summary,
            last_burst,
            next_signal,
        },
        readings: SignalReadings {
            sent: header.tokens_sent,
            received: header.tokens_received,
            ratio,
            peak,
            largest,
            quiet_s,
        },
        unit,
        selected,
        usual_so_far_tokens: {
            let sum: u64 = rows
                .iter()
                .filter(|r| !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped))
                .filter_map(|r| tape_row_usual_tokens(&r.path, plan, header))
                .sum();
            (sum > 0).then_some(sum)
        },
        last_finished: last_finished_row(rows, flat, plan, run_start),
    }
}

/// Screen 3's own model (issue #632): the by-store treemap for
/// whichever [`crate::treemap::Measure`] is currently selected, the
/// "largest stores" table and its footer, and the "tokens over time"
/// stacked chart's own per-phase series. A caller builds this only
/// while the Cost screen is actually on display (the same
/// performance rule the overview's own model follows) -- unlike the
/// overview, this is also rebuilt on every measure switch (`i`/`o`/
/// `$`) and zoom, both of which only ever change `measure`/how the
/// already-built `tree` is sliced, never trigger a fresh
/// `RunModel::observe` of their own.
#[derive(Debug, Clone, PartialEq)]
pub struct CostScreen {
    pub measure: crate::treemap::Measure,
    /// The root's own children, already chain-folded -- the
    /// top-level tiles before any zoom (DESIGN.md: "the zoomed
    /// store's path is shown as a breadcrumb above the treemap",
    /// `oscilloscope`'s own `App::cost_zoom` decides which node's
    /// children are actually on screen this frame).
    pub tree: Vec<crate::treemap::TreemapNode>,
    /// How many *finished* stores were left out for having no value
    /// under `measure` at all (DESIGN.md: "the footer counts them").
    pub stores_without_measure: u64,
    pub stores: crate::cost::LargestStores,
    /// One series per top-level phase, in the phase strip's own
    /// order (DESIGN.md: "one band per top-level step").
    pub tokens_over_time: Vec<(String, Vec<u64>)>,
    /// The exact start/end the chart's own buckets span -- the same
    /// pair [`tokens_over_time_by_phase`](crate::cost::tokens_over_time_by_phase)
    /// was built from, so a front end's own clock-axis labels always
    /// name the span the bars actually cover, never a separate
    /// "now" of its own that could disagree with it. `chart_start_ts`
    /// is `None` only before the run has a start time at all, in
    /// which case every bucket is already empty too.
    pub chart_start_ts: Option<String>,
    pub chart_end_ts: String,
}

#[allow(clippy::too_many_arguments)]
pub fn build_cost_screen(
    rows: &[Row],
    header: &crate::render::Header,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, crate::model::RowStatus>,
    kinds: &BTreeMap<String, RowKind>,
    state: Option<&Value>,
    measure: crate::treemap::Measure,
    model: &RunModel,
    now: &str,
    // Issue #641: the front end's own number of distinct glyphs for
    // the "tokens over time" chart's bands -- `crate::cost::cap_
    // phase_bands`' own `max_bands`, kept here rather than hardcoded
    // so core never has to know a glyph count of its own.
    max_phase_bands: usize,
) -> CostScreen {
    let (tree, stores_without_measure) =
        crate::treemap::build_tree_for_measure("prime", flat, statuses, kinds, measure);
    let stores = crate::cost::largest_stores("prime", flat, statuses, kinds, state);
    let phases: Vec<(String, String)> = build_phase_strip(rows)
        .into_iter()
        .map(|p| (p.path, p.label))
        .collect();
    // DESIGN.md: the chart spans "the run's start to now (or to its
    // end)" -- once the run has actually finished, `run_end_ts` is
    // that fixed end, so a finished run's chart (`osi watch` on a
    // document that is done) stops growing new buckets as real clock
    // time keeps advancing underneath it.
    let end = header.run_end_ts.as_deref().unwrap_or(now);
    let tokens_over_time = crate::cost::cap_phase_bands(
        crate::cost::tokens_over_time_by_phase(
            header.run_start_ts.as_deref(),
            end,
            flat,
            &phases,
            model,
        ),
        max_phase_bands,
    );
    CostScreen {
        measure,
        tree,
        stores_without_measure,
        stores,
        tokens_over_time,
        chart_start_ts: header.run_start_ts.clone(),
        chart_end_ts: end.to_string(),
    }
}

/// A decision row's own detail, for the selected pane when the
/// flow pane's own selection is a `◆`/untaken-branch row rather than
/// an ordinary step (issue #635: "a decision row shows its condition,
/// its mode, the branch it took, and the model's answer for a
/// model-mode condition").
/// A selected decision station's own detail (DESIGN.md §2.3): its
/// condition, the branch it took, and the model's own answer for a
/// model-mode condition.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedDecision {
    pub label: String,
    pub condition: Option<crate::plan::ConditionInfo>,
    pub branch: Option<String>,
    pub inferred: bool,
    pub nothing_ran: bool,
    pub answer: Option<String>,
}

/// One named step inside the selected pass (DESIGN.md §2.3's
/// `Pass 1 · cats · Selected` pane): `Draft` -- a capitalised step
/// name, its own status, tokens and deviation, and (once done) what
/// it produced.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedPassStep {
    pub label: String,
    pub status: RowStatus,
    pub sent: u64,
    pub received: u64,
    pub usual: Option<u64>,
    pub produced: Option<String>,
}

/// The selected loop pass (DESIGN.md §2.3, §5's "the selected pass
/// or station pane"): `index`/`label` name it (`Pass 1 · cats`),
/// `active_*`/`slot` describe whichever of its own steps is
/// currently live (or, once the pass has finished, its own last
/// step) for the Status line, and `steps` is every prompt the pass
/// ran, for its own named row and the Produced text.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedPass {
    pub index: usize,
    pub label: String,
    pub active_label: String,
    pub active_status: RowStatus,
    pub active_elapsed_s: Option<f64>,
    pub slot: Option<usize>,
    pub steps: Vec<SelectedPassStep>,
}

/// Screen 2's own model (issue #691, part 3 of #677) -- built only
/// while the Flow screen is actually on display, the same
/// performance rule `build_cost_screen` already follows for screen
/// 3. `rail` is [`crate::rail::build_rail`]'s own output; `story` is
/// `crate::flow::build_story`'s; `selected_pass`/`selected_step` pick
/// whichever of the two the current selection actually is -- never
/// both, a loop pass and a plain station are never the same
/// selection at once.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowScreen {
    pub rail: crate::rail::Rail,
    pub unit: u64,
    pub story: Vec<crate::flow::StoryEntry>,
    pub selected_pass: Option<SelectedPass>,
    pub selected_decision: Option<SelectedDecision>,
    pub selected_step: Option<FocusedStep>,
    /// §3.2's status line: the newest finished step, shared with
    /// the Signal's own.
    pub last_finished: Option<SignalLastFinished>,
    /// The Totals row's own deviation (§3.3): the sum of every
    /// row's usual tokens that has actually run so far, same as the
    /// Tape's own -- comparing against the whole run's eventual
    /// usual instead would read wildly off on any partial run.
    pub usual_so_far_tokens: Option<u64>,
}

/// The first pass of the first loop that is either running or, with
/// nothing running, simply first in plan order -- the rail's own
/// default selection (DESIGN.md's own frames: `Pass 1 · cats` is
/// selected with no explicit selection at all).
/// A loop's own story entry (DESIGN.md §2.3: `⟳ drafts · 3 passes
/// · 2 wide`) -- `--log` has no such line (a loop writes no event
/// of its own for *starting*), so this synthesises one, the same
/// way an unnamed `if`'s own inferred decision already is.
fn loop_story_lines(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
) -> Vec<crate::diff::LogLine> {
    rows.iter()
        .filter(|r| r.kind == RowKind::Loop)
        .filter_map(|r| {
            let total = r.loop_progress.and_then(|p| p.total)?;
            let wide = plan.match_path(&r.path).and_then(|m| {
                m.entries.iter().find_map(|e| match &e.kind {
                    PlanEntryKind::Loop {
                        max_concurrency, ..
                    } => *max_concurrency,
                    _ => None,
                })
            })?;
            let ts = flat.get(&r.path).and_then(|n| n.created_at.clone());
            Some(crate::diff::LogLine {
                ts,
                text: format!("⟳ {} · {total} passes · {wide} wide", r.label),
                kind: crate::diff::LogKind::Report,
                path: Some(r.path.clone()),
            })
        })
        .collect()
}

fn default_pass_path(rows: &[Row]) -> Option<&str> {
    let running = rows
        .iter()
        .find(|r| r.kind == RowKind::Pass && r.status.kind == StatusKind::Running);
    running
        .or_else(|| rows.iter().find(|r| r.kind == RowKind::Pass))
        .map(|r| r.path.as_str())
}

fn capitalize(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_selected_pass(
    pass_path: &str,
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    header: &Header,
    state: Option<&Value>,
) -> Option<SelectedPass> {
    let pass_row = rows.iter().find(|r| r.path == pass_path)?;
    let loop_path = pass_path.rsplit_once('.').map(|(p, _)| p)?;
    let index_str = pass_path.rsplit('.').next()?.strip_prefix("iter_")?;
    let pass_index: u64 = index_str.parse().ok()?;
    let label = crate::rail::pass_each_value(loop_path, pass_index, plan, state)
        .unwrap_or_else(|| format!("pass {}", pass_index + 1));
    let children: Vec<&Row> = rows
        .iter()
        .filter(|r| r.depth == pass_row.depth + 1 && r.path.starts_with(&format!("{pass_path}.")))
        .collect();
    let active = children
        .iter()
        .find(|r| {
            matches!(
                r.status.kind,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            )
        })
        .or_else(|| children.last())
        .copied();
    let slot = active.and_then(|active| {
        if active.status.kind != StatusKind::Running {
            return None;
        }
        let siblings: Vec<&Row> = rows
            .iter()
            .filter(|r| {
                r.kind == RowKind::Pass
                    && r.path.starts_with(&format!("{loop_path}."))
                    && r.status.kind == StatusKind::Running
            })
            .collect();
        let mut ordered: Vec<(&str, Option<String>)> = siblings
            .iter()
            .map(|r| {
                (
                    r.path.as_str(),
                    flat.get(&r.path).and_then(|n| n.created_at.clone()),
                )
            })
            .collect();
        ordered.sort_by(|a, b| a.1.cmp(&b.1));
        ordered
            .iter()
            .position(|(p, _)| *p == pass_path)
            .map(|i| i + 1)
    });
    let steps = children
        .iter()
        .filter(|r| r.kind == RowKind::Prompt)
        .map(|r| {
            let (sent, received) = crate::model::subtree_tokens(&r.path, flat);
            let usual = tape_row_usual_tokens(&r.path, plan, header);
            let produced = flat
                .get(&r.path)
                .and_then(|n| n.value.as_str())
                .map(str::to_string);
            SelectedPassStep {
                label: capitalize(&r.label),
                status: r.status.clone(),
                sent,
                received,
                usual,
                produced,
            }
        })
        .collect();
    Some(SelectedPass {
        index: (pass_index + 1) as usize,
        label,
        active_label: active.map(|r| r.label.clone()).unwrap_or_default(),
        active_status: active.map(|r| r.status.clone()).unwrap_or(RowStatus {
            kind: StatusKind::Pending,
            skip_reason: None,
            retrying: false,
        }),
        active_elapsed_s: active.and_then(|r| r.duration_s),
        slot,
        steps,
    })
}

/// `rows` is always `render::RenderState::unfolded_rows`, never
/// `RenderState::rows` -- the tree screen's own folding plays no
/// part in the rail (`rail::build_rail`'s own doc comment).
#[allow(clippy::too_many_arguments)]
pub fn build_flow(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    header: &Header,
    state: Option<&Value>,
    story_lines: &[crate::diff::LogLine],
    selected_path: Option<&str>,
    zone: &dyn ClockZone,
    run_ended_at: Option<&str>,
    now: &str,
) -> FlowScreen {
    let rail = crate::rail::build_rail(rows, flat, plan, header, state, zone);
    let largest_usual_step = header
        .history_step_usual
        .values()
        .filter_map(|s| s.tokens_p50)
        .max();
    let unit = tape_unit(largest_usual_step);
    let mut story_lines_with_loops = story_lines.to_vec();
    story_lines_with_loops.extend(loop_story_lines(rows, flat, plan));
    let story = crate::flow::build_story(&story_lines_with_loops, &[], zone);
    // The default pass only stands in for the selection when there
    // is none at all, or when the selection already names one of a
    // loop's own passes -- never for a selection that lands on some
    // other station or decision entirely, which must show its own
    // pane instead (issue #691's own follow-up: a loop existing on
    // the rail must not make every other station's own pane
    // unreachable).
    let pass_path = match selected_path {
        None => default_pass_path(rows).map(str::to_string),
        Some(p) if rows.iter().any(|r| r.path == p && r.kind == RowKind::Pass) => {
            Some(p.to_string())
        }
        Some(_) => None,
    };
    let selected_pass =
        pass_path.and_then(|p| build_selected_pass(&p, rows, flat, plan, header, state));
    let mut selected_decision = None;
    let mut selected_step = None;
    if selected_pass.is_none() {
        if let Some(row) = selected_path.and_then(|path| rows.iter().find(|r| r.path == path)) {
            if row.kind == RowKind::If {
                let node = flat.get(&row.path);
                let branch = node.and_then(|n| n.branch.clone());
                let answer = node.and_then(|n| n.meta.get("answer")).map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
                let condition = plan.match_path(&row.path).and_then(|m| {
                    m.entries.iter().find_map(|e| match &e.kind {
                        PlanEntryKind::If {
                            condition: Some(c), ..
                        } => Some(c.clone()),
                        _ => None,
                    })
                });
                selected_decision = Some(SelectedDecision {
                    label: row.label.clone(),
                    condition,
                    branch,
                    inferred: false,
                    nothing_ran: false,
                    answer,
                });
            } else {
                let mut step = build_focused_step(row, flat, plan, model, state, now, run_ended_at);
                // A sequential container's own child keeps its own
                // ten-cell station name on the rail, but the Selected
                // pane's own title still says which container it's
                // inside (`gate › reply`), the same breadcrumb the
                // status line already reads through `tape_row_label`.
                step.breadcrumb = tape_row_label(row, flat, plan);
                selected_step = Some(step);
            }
        }
    }
    let last_finished = last_finished_row(rows, flat, plan, header.run_start_ts.as_deref());
    let usual_so_far: u64 = rows
        .iter()
        .filter(|row| {
            !matches!(
                row.kind,
                RowKind::Root
                    | RowKind::Pass
                    | RowKind::QuickSteps
                    | RowKind::Report
                    | RowKind::Unknown
            ) && !matches!(row.status.kind, StatusKind::Pending | StatusKind::Skipped)
        })
        .filter_map(|row| tape_row_usual_tokens(&row.path, plan, header))
        .sum();
    let usual_so_far_tokens = (usual_so_far > 0).then_some(usual_so_far);
    FlowScreen {
        rail,
        unit,
        story,
        selected_pass,
        selected_decision,
        selected_step,
        last_finished,
        usual_so_far_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcessState;
    use crate::plan::PlanTree;
    use chrono::FixedOffset;
    use serde_json::json;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn base_header() -> crate::render::Header {
        crate::render::Header {
            document: "feature-0417.yml".to_string(),
            engine: "cof".to_string(),
            run_id: None,
            status: RunStatus::Running,
            elapsed_s: 2877.0,
            run_start_ts: Some("2026-10-08T11:04:58Z".to_string()),
            run_end_ts: None,
            effects_done: 5,
            effects_planned: Some(12),
            effects_planned_is_lower_bound: false,
            effects_planned_full: None,
            tokens_sent: 1000,
            tokens_received: 500,
            tokens_sent_live_estimate: 0,
            tokens_received_live_estimate: 0,
            innermost_loop_eta_s: Some(360.0),
            innermost_loop_eta_ts: Some("2026-10-08T11:58:00Z".to_string()),
            history_eta_ts: None,
            history_eta_past_usual: false,
            history_eta_remaining_s: None,
            history_eta_low_ts: None,
            history_eta_high_ts: None,
            history_vs_usual_s: None,
            history_usual_total_s: None,
            history_usual_total_tokens: None,
            history_same_hash_count: None,
            history_step_usual: Default::default(),
            latest_report: None,
            engine_alive: true,
        }
    }

    #[test]
    fn screen_from_key_only_accepts_available_screens() {
        assert_eq!(Screen::from_key('0'), Some(Screen::Tape));
        assert_eq!(Screen::from_key('1'), Some(Screen::Signal));
        assert_eq!(Screen::from_key('2'), Some(Screen::Flow));
        assert_eq!(Screen::from_key('3'), Some(Screen::Cost));
        assert_eq!(Screen::from_key('x'), None);
    }

    // -------------------------------------------------------------
    // The Tape (issue #678)
    // -------------------------------------------------------------

    #[test]
    fn tape_unit_picks_the_smallest_scale_that_fits_eight_cells() {
        assert_eq!(tape_unit(None), 50);
        assert_eq!(tape_unit(Some(0)), 10);
        assert_eq!(tape_unit(Some(80)), 10);
        // The design's own fixture history: `prime.summary`'s usual
        // 402 t rounds to an 8-cell bar at 50 t/cell (`round(402.0 /
        // 50.0) == 8.0`), same as the bar itself draws, so the unit
        // stays 50 for it -- every `tape--*` frame with history
        // keeps its own fixed-looking `▰ = 50 t` title because of
        // this, not in spite of it.
        assert_eq!(tape_unit(Some(402)), 50);
        assert_eq!(tape_unit(Some(1600)), 200);
        assert_eq!(tape_unit(Some(4000)), 500);
        assert_eq!(tape_unit(Some(100_000)), 500);
    }

    /// Past the point a 50-token unit's own bar would round up to a
    /// *ninth* cell (`round(425.0 / 50.0) == 9.0`, half rounding away
    /// from zero same as every other `.round()` here), the next
    /// scale takes over.
    #[test]
    fn tape_unit_moves_to_the_next_scale_once_50_would_overflow_the_bar() {
        assert_eq!(tape_unit(Some(424)), 50);
        assert_eq!(tape_unit(Some(425)), 100);
    }

    fn tape_row(path: &str, depth: usize, label: &str, kind: RowKind, status: StatusKind) -> Row {
        Row {
            path: path.to_string(),
            depth,
            label: label.to_string(),
            kind,
            status: RowStatus {
                kind: status,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: kind != RowKind::Tool && kind != RowKind::Prompt,
        }
    }

    fn tape_node(created_at: &str, completed_at: Option<&str>) -> NodeMeta {
        NodeMeta {
            created_at: Some(created_at.to_string()),
            completed_at: completed_at.map(str::to_string),
            error: None,
            branch: None,
            progress_done: None,
            progress_total: None,
            progress_eta_s: None,
            live: None,
            prompt_live: None,
            value: Value::Null,
            meta: json!({}),
        }
    }

    /// A demo.yml-shaped moment (§2.1): two finished leaves, a
    /// two-slot tree loop with one finished pass and one still
    /// running, and one pass ahead -- the same shape the
    /// `tape--normal--120x30` frame freezes. Checks the ordering
    /// rule (finished rows above the Now band, in finish order;
    /// running leaves below it; a dispatched loop stays above even
    /// though it hasn't finished) and the slots rule's own figures.
    #[test]
    fn build_tape_orders_rows_around_the_now_band_and_computes_the_slots_rule() {
        let mut rows = vec![
            tape_row("prime.kind", 1, "kind", RowKind::Prompt, StatusKind::Done),
            tape_row(
                "prime.drafts",
                1,
                "drafts",
                RowKind::Loop,
                StatusKind::Running,
            ),
            tape_row(
                "prime.drafts.iter_0.draft",
                3,
                "iter_0 › draft",
                RowKind::Prompt,
                StatusKind::Done,
            ),
            tape_row(
                "prime.drafts.iter_0.pause",
                3,
                "iter_0 › pause",
                RowKind::Tool,
                StatusKind::Running,
            ),
            tape_row(
                "prime.drafts.iter_1.draft",
                3,
                "iter_1 › draft",
                RowKind::Prompt,
                StatusKind::Pending,
            ),
        ];
        rows[1].loop_progress = Some(crate::render::LoopProgress {
            done: 0,
            total: Some(2),
            eta_s: None,
        });
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.kind".to_string(),
            tape_node("2026-01-01T00:00:00Z", Some("2026-01-01T00:00:01Z")),
        );
        flat.insert(
            "prime.drafts".to_string(),
            tape_node("2026-01-01T00:00:02Z", None),
        );
        flat.insert(
            "prime.drafts.iter_0.draft".to_string(),
            tape_node("2026-01-01T00:00:02Z", Some("2026-01-01T00:00:02Z")),
        );
        flat.insert(
            "prime.drafts.iter_0.pause".to_string(),
            tape_node("2026-01-01T00:00:02Z", None),
        );

        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        header.status = RunStatus::Running;

        let plan = PlanTree::empty();
        let model = RunModel::new();
        let tape = build_tape(
            &rows,
            &header,
            &flat,
            &plan,
            &model,
            None,
            None,
            true,
            "2026-01-01T00:00:02Z",
        );

        // Finished rows (kind, the dispatched-but-unfinished loop,
        // its one finished pass) all sit above the Now band, in
        // finish/dispatch order; the running pause is below it; the
        // pending sibling pass is ahead, after the slots rule.
        let order: Vec<String> = tape
            .items
            .iter()
            .map(|item| match item {
                TapeItem::RunStarted { .. } => "start".to_string(),
                TapeItem::Row(r) => r.path.clone(),
                TapeItem::NowBand { .. } => "now".to_string(),
                TapeItem::SlotsRule { .. } => "slots".to_string(),
                TapeItem::RunEnd { .. } => "end".to_string(),
                TapeItem::HeadRule => "head_rule".to_string(),
            })
            .collect();
        assert_eq!(
            order,
            vec![
                "start",
                "prime.kind",
                "prime.drafts",
                "prime.drafts.iter_0.draft",
                "now",
                "prime.drafts.iter_0.pause",
                "slots",
                "prime.drafts.iter_1.draft",
                "end",
            ]
        );
        match &tape.items[6] {
            TapeItem::SlotsRule {
                busy,
                slots,
                queued,
            } => {
                assert_eq!(*busy, 1);
                assert_eq!(*slots, 1); // no dispatch event in this test: falls back to `busy`
                assert_eq!(*queued, 1);
            }
            other => panic!("expected a slots rule, got {other:?}"),
        }
        assert_eq!(tape.done, 2); // `kind` and the loop's own finished pass, `iter_0 › draft`
        assert_eq!(tape.running, 1);
        assert_eq!(tape.ahead, 1);
    }

    #[test]
    fn tape_row_is_under_a_queued_pass_matches_a_pass_or_its_own_leaf() {
        assert!(tape_row_is_under_a_queued_pass("prime.drafts.iter_2"));
        assert!(tape_row_is_under_a_queued_pass("prime.drafts.iter_2.draft"));
        assert!(!tape_row_is_under_a_queued_pass("prime.drafts"));
        assert!(!tape_row_is_under_a_queued_pass("prime.summary"));
    }

    /// A queued ahead prompt's own preview line (§2.1) reads the
    /// plan's unrendered template -- it has no `meta.prompt_sent` of
    /// its own yet, since it hasn't run.
    #[test]
    fn build_tape_previews_a_queued_prompts_own_template() {
        let plan = tree_loop_with_prompt_plan();
        let row = tape_row(
            "prime.fan.iter_0.ask",
            2,
            "iter_0 › ask",
            RowKind::Prompt,
            StatusKind::Pending,
        );
        let flat = BTreeMap::new();
        let model = RunModel::new();
        let (what, preview, _tokens) = tape_row_content(
            &row,
            &flat,
            &plan,
            &model,
            None,
            true,
            false,
            "2026-10-08T11:52:55Z",
        );
        assert_eq!(what, "prompt");
        assert_eq!(preview.as_deref(), Some("› hi"));
    }

    fn running_prompt_row() -> Row {
        let mut row = tape_row("prime.ask", 1, "ask", RowKind::Prompt, StatusKind::Running);
        row.duration_s = Some(1.0);
        row
    }

    /// §2.1's running-prompt anatomy (issue #675): before the first
    /// delta, the row reads "waiting for the first token", not a
    /// bogus `↑0 ↓0`.
    #[test]
    fn tape_row_content_shows_waiting_for_the_first_token() {
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                    "adapter": "ollama", "model": "m", "prompt_chars": 40,
                    "live": {"v": 1, "attempt": 0, "adapter": "ollama", "model": "m",
                        "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
                        "streaming": true, "first_token_at": null,
                        "tokens_sent": null, "tokens_sent_estimate": 10, "tokens_received": null,
                        "tokens_received_estimate": null, "chars_received": 0,
                        "tokens_per_second": null}}}
            }}),
        );
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let (what, _preview, tokens) = tape_row_content(
            &running_prompt_row(),
            &flat,
            &plan,
            &model,
            None,
            false,
            false,
            "2026-10-08T11:46:44.2Z",
        );
        assert_eq!(what, "1.2s · waiting for the first token");
        assert!(tokens.live);
    }

    /// Mid-stream with estimates only (the Ollama shape): the row
    /// reads the received estimate marked `≈` and the rate.
    #[test]
    fn tape_row_content_shows_a_mid_stream_estimate_and_rate() {
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                    "adapter": "ollama", "model": "m", "prompt_chars": 40,
                    "live": {"v": 1, "attempt": 0, "adapter": "ollama", "model": "m",
                        "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
                        "streaming": true, "first_token_at": "2026-10-08T11:46:43.6Z",
                        "tokens_sent": null, "tokens_sent_estimate": 10, "tokens_received": null,
                        "tokens_received_estimate": 41, "chars_received": 160,
                        "tokens_per_second": 68.0}}}
            }}),
        );
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let (what, _preview, tokens) = tape_row_content(
            &running_prompt_row(),
            &flat,
            &plan,
            &model,
            None,
            false,
            false,
            "2026-10-08T11:46:44Z",
        );
        assert_eq!(what, "1.0s · ↓≈41 t · 68 t/s");
        assert_eq!(tokens.received, Some(41));
        assert!(tokens.live);
    }

    /// Mid-stream with exact input tokens (the Anthropic shape:
    /// `tokens_sent` set at `message_start`) -- the token column's
    /// own sent figure is exact, never `≈`, while received stays an
    /// estimate.
    #[test]
    fn tape_row_content_shows_an_exact_sent_count_mid_stream() {
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                    "adapter": "anthropic", "model": "m", "prompt_chars": 1400,
                    "live": {"v": 1, "attempt": 0, "adapter": "anthropic", "model": "m",
                        "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
                        "streaming": true, "first_token_at": "2026-10-08T11:46:43.7Z",
                        "tokens_sent": 348, "tokens_sent_estimate": 350, "tokens_received": null,
                        "tokens_received_estimate": 5, "chars_received": 20,
                        "tokens_per_second": null}}}
            }}),
        );
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let (_what, _preview, tokens) = tape_row_content(
            &running_prompt_row(),
            &flat,
            &plan,
            &model,
            None,
            false,
            false,
            "2026-10-08T11:46:44Z",
        );
        assert_eq!(tokens.sent, Some(348));
        assert_eq!(tokens.received, Some(5));
    }

    /// A non-streaming adapter's own running row keeps today's bare
    /// elapsed-time shape -- `streaming: false` means only the sent
    /// estimate and the clock are live, so there is nothing else to
    /// show.
    #[test]
    fn tape_row_content_shows_only_elapsed_for_a_non_streaming_adapter() {
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                    "adapter": "litellm", "model": "m", "prompt_chars": 40,
                    "live": {"v": 1, "attempt": 0, "adapter": "litellm", "model": "m",
                        "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
                        "streaming": false, "first_token_at": null,
                        "tokens_sent": null, "tokens_sent_estimate": 10, "tokens_received": null,
                        "tokens_received_estimate": null, "chars_received": null,
                        "tokens_per_second": null}}}
            }}),
        );
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let (what, _preview, _tokens) = tape_row_content(
            &running_prompt_row(),
            &flat,
            &plan,
            &model,
            None,
            false,
            false,
            "2026-10-08T11:46:44Z",
        );
        assert_eq!(what, "1.0s");
    }

    /// A second attempt after a retry appends its own number.
    #[test]
    fn tape_row_content_appends_the_attempt_number_on_a_retry() {
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                    "adapter": "ollama", "model": "m", "prompt_chars": 40,
                    "live": {"v": 1, "attempt": 1, "adapter": "ollama", "model": "m",
                        "started_at": "2026-10-08T11:46:50Z", "updated_at": "2026-10-08T11:46:51Z",
                        "streaming": true, "first_token_at": null,
                        "tokens_sent": null, "tokens_sent_estimate": 10, "tokens_received": null,
                        "tokens_received_estimate": null, "chars_received": 0,
                        "tokens_per_second": null}}}
            }}),
        );
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let (what, _preview, _tokens) = tape_row_content(
            &running_prompt_row(),
            &flat,
            &plan,
            &model,
            None,
            false,
            false,
            "2026-10-08T11:46:51Z",
        );
        assert_eq!(what, "1.0s · attempt 2 · waiting for the first token");
    }

    /// A node whose `meta.live` is malformed (or missing entirely)
    /// shows as today, never a crash -- the elapsed time alone, since
    /// there is nothing live to read.
    #[test]
    fn tape_row_content_falls_back_to_elapsed_with_no_live_data_at_all() {
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                    "adapter": "ollama", "model": "m", "prompt_chars": 40,
                    "live": "not an object"}}
            }}),
        );
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let (what, _preview, tokens) = tape_row_content(
            &running_prompt_row(),
            &flat,
            &plan,
            &model,
            None,
            false,
            false,
            "2026-10-08T11:46:44Z",
        );
        assert_eq!(what, "1.0s");
        assert_eq!(tokens, TapeTokens::default());
    }

    #[test]
    fn header_line_has_no_truncation_at_120_columns() {
        let header = base_header();
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            120,
            true,
            0,
            Some(Screen::Tape),
        );
        let text = header_line_text(&segments);
        assert!(text.contains("feature-0417.yml"));
        assert!(text.contains("running"));
        assert!(text.contains("47m57s"));
        assert!(text.contains("11:52:55"));
        assert!(text.contains("loop ETA ~11:58"));
        assert!(text.contains("[0 tape]"));
        assert!(text.contains("2 flow"));
        assert_eq!(text.chars().count(), segments_width(&segments));
    }

    #[test]
    fn header_line_shows_the_history_eta_in_place_of_the_loop_eta() {
        let mut header = base_header();
        header.history_eta_ts = Some("2026-10-08T12:05:00Z".to_string());
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            120,
            true,
            0,
            Some(Screen::Tape),
        );
        let text = header_line_text(&segments);
        assert!(text.contains("ETA ~12:05"), "{text}");
        assert!(!text.contains("loop ETA"), "{text}");
    }

    #[test]
    fn header_line_shows_eta_question_mark_once_past_the_usual_end() {
        let mut header = base_header();
        header.history_eta_past_usual = true;
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            120,
            true,
            0,
            Some(Screen::Tape),
        );
        let text = header_line_text(&segments);
        assert!(text.contains("ETA ?"), "{text}");
        assert!(!text.contains("loop ETA"), "{text}");
    }

    #[test]
    fn header_line_shows_a_report_in_place_of_a_run_id() {
        let mut header = base_header();
        header.latest_report = Some(crate::render::HeaderReport {
            level: crate::observe::ReportLevel::Info,
            message: "fixing 1 blocker".to_string(),
            ts: "2026-10-08T11:52:00Z".to_string(),
        });
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            120,
            true,
            0,
            Some(Screen::Tape),
        );
        let text = header_line_text(&segments);
        assert!(text.contains("\u{25b8} fixing 1 blocker"));
        assert!(!text.contains("run_id"));
    }

    #[test]
    fn header_line_shrinks_tabs_to_numbers_first() {
        let header = base_header();
        let wide = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            120,
            true,
            0,
            Some(Screen::Tape),
        );
        let wide_text = header_line_text(&wide);
        let narrow = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            100,
            true,
            0,
            Some(Screen::Tape),
        );
        let narrow_text = header_line_text(&narrow);
        assert!(narrow_text.chars().count() <= wide_text.chars().count());
        // Status/elapsed/clock survive even once the line had to
        // shrink the tabs or anything else.
        assert!(narrow_text.contains("running"));
        assert!(narrow_text.contains("11:52:55"));
    }

    fn long_header() -> crate::render::Header {
        let mut header = base_header();
        header.document = "a-fairly-long-orchestration-document-name.yml".to_string();
        header.latest_report = Some(crate::render::HeaderReport {
            level: crate::observe::ReportLevel::Info,
            message: "a somewhat long report message that will not fit".to_string(),
            ts: "2026-10-08T11:52:00Z".to_string(),
        });
        header
    }

    #[test]
    fn header_line_truncates_report_before_document_at_80_columns() {
        let header = long_header();
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            80,
            true,
            0,
            Some(Screen::Tape),
        );
        let text = header_line_text(&segments);
        // The report is already down to a bare `▸` (step 2) before the
        // document starts giving up its own head (step 3) -- and the
        // ETA (step 4) survives both, since this width doesn't need
        // it dropped too.
        assert_eq!(
            text,
            "⠔⠒⠒⠤⠤⠔⠒⠒ …yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58 · ▸   [0]  1  2  3"
        );
    }

    #[test]
    fn header_line_drops_eta_before_status_at_50_columns() {
        let header = long_header();
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            50,
            true,
            0,
            Some(Screen::Tape),
        );
        let text = header_line_text(&segments);
        // Squeezed enough to drop the document to nothing and the
        // report to a bare `▸` and the ETA entirely -- but status,
        // elapsed and the clock are all still there, untruncated.
        assert_eq!(
            text,
            "⠔⠒⠒⠤⠤⠔⠒⠒  · ♥ running 47m57s · 11:52:55 · ▸   [0]  1  2  3"
        );
    }

    #[test]
    fn header_line_truncation_order_holds_at_every_required_width_without_a_report() {
        let header = base_header();
        let expected = [
            (
                50u16,
                "⠔⠒⠒⠤⠤⠔⠒⠒  · ♥ running 47m57s · 11:52:55   [0]  1  2  3",
            ),
            (
                80,
                "⠔⠒⠒⠤⠤⠔⠒⠒ …417.yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58   [0]  1  2  3",
            ),
            (
                100,
                "⠔⠒⠒⠤⠤⠔⠒⠒ feature-0417.yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58   [0]  1  2  3",
            ),
            (
                120,
                "⠔⠒⠒⠤⠤⠔⠒⠒ feature-0417.yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58   [0 tape]  1 signal  2 flow  3 cost",
            ),
        ];
        for (width, want) in expected {
            let segments = header_line(
                &header,
                "2026-10-08T11:52:55Z",
                &utc(),
                width,
                true,
                0,
                Some(Screen::Tape),
            );
            assert_eq!(header_line_text(&segments), want, "at {width} columns");
        }
    }

    #[test]
    fn header_line_truncation_order_holds_at_every_required_width_with_a_report() {
        let mut header = base_header();
        header.latest_report = Some(crate::render::HeaderReport {
            level: crate::observe::ReportLevel::Info,
            message: "fixing 1 blocker".to_string(),
            ts: "2026-10-08T11:52:00Z".to_string(),
        });
        let expected = [
            (
                50u16,
                "⠔⠒⠒⠤⠤⠔⠒⠒  · ♥ running 47m57s · 11:52:55 · ▸   [0]  1  2  3",
            ),
            (
                80,
                "⠔⠒⠒⠤⠤⠔⠒⠒ …yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58 · ▸   [0]  1  2  3",
            ),
            (
                100,
                "⠔⠒⠒⠤⠤⠔⠒⠒ feature-0417.yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58 · ▸ fixing…   [0]  1  2  3",
            ),
            (
                120,
                "⠔⠒⠒⠤⠤⠔⠒⠒ feature-0417.yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58 · ▸ fixing 1 blocker   [0]  1  2  3",
            ),
        ];
        for (width, want) in expected {
            let segments = header_line(
                &header,
                "2026-10-08T11:52:55Z",
                &utc(),
                width,
                true,
                0,
                Some(Screen::Tape),
            );
            assert_eq!(header_line_text(&segments), want, "at {width} columns");
        }
    }

    #[test]
    fn header_line_with_no_screen_omits_the_tabs() {
        let header = base_header();
        let segments = header_line(&header, "2026-10-08T11:52:55Z", &utc(), 120, true, 0, None);
        let text = header_line_text(&segments);
        assert!(!text.contains("overview"));
        assert!(!text.contains("flow"));
        assert!(
            segments
                .iter()
                .all(|s| !matches!(s.role, HeaderRole::TabActive | HeaderRole::TabInactive))
        );
    }

    #[test]
    fn header_line_no_animation_uses_a_static_dot() {
        let header = base_header();
        let segments = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            120,
            false,
            7,
            Some(Screen::Tape),
        );
        assert_eq!(segments[0].role, HeaderRole::Icon);
        assert_eq!(segments[0].text, "\u{25c9}");
    }

    fn program_with_ifs() -> electricity_bytecode::Program {
        // A directory per call, not per process: tests run on parallel
        // threads, and each call removes its own directory when it's done.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("osi-screens-test-{}-{call}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("doc.yml");
        std::fs::write(
            &path,
            r#"effects:
  - name: plan
    type: prompt
    provider: scripted
    template: "plan"
  - name: check
    type: if
    if:
      mode: cel
      expr: "true"
    then:
      - name: taken
        type: tool
        provider: shell
        params:
          command: echo
          args: ["ok"]
    else:
      - name: untaken
        type: tool
        provider: shell
        params:
          command: echo
          args: ["no"]
"#,
        )
        .unwrap();
        let program = crate::plan::compile(&path, None, &indexmap::IndexMap::new())
            .expect("document compiles");
        std::fs::remove_dir_all(&tmp).ok();
        program
    }

    fn flat_from(json: Value) -> BTreeMap<String, NodeMeta> {
        crate::model::flatten_state(&json)
    }

    /// A named tree loop (`fan`, `max_concurrency: 2`) with one
    /// prompt body (`ask`) -- issue #631's own now-pane fix needs a
    /// step that is confirmed running by `--events` alone, before it
    /// ever gets a state node of its own (DESIGN.md §1.4), which only
    /// a tree-loop pass's prompt can be.
    fn tree_loop_with_prompt_plan() -> PlanTree {
        use electricity_bytecode::{
            EffectPath, Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op,
            Program, PromptContent, PromptOp, PromptType, Region, TemplateText,
        };
        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let ask_path = pass_path.push_name("ask");
        let ask = Op {
            path: ask_path,
            name: Some("ask".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", false, Escape::None)),
                prompt_type: PromptType::Text,
                schema: None,
                model: Some("scripted-model".to_string()),
                provider: Some("scripted".to_string()),
                provider_fallbacks: Vec::new(),
                routing_override: None,
                model_params: None,
                timeout_ms: None,
                deterministic: false,
                inputs: None,
                assets: Vec::new(),
                retries: None,
                description: None,
                group: None,
                split_budget_tokens: None,
                split_enabled: None,
                split_max_parts: None,
                split_separator: None,
                split_merge_template: None,
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: loop_path,
                    name: Some("fan".to_string()),
                    kind: NodeKind::Control(Region::Loop {
                        name: Some("fan".to_string()),
                        spec: LoopSpec::Each {
                            in_path: "prime.items".to_string(),
                            as_name: "item".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![ask],
                            overlay: true,
                        }),
                        flow: LoopFlow::Tree,
                        max_concurrency: Some(2),
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
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// Issue #631: a step known only from `--events` (a tree-loop
    /// pass's prompt, confirmed running before it ever gets a state
    /// node of its own) shows `since`/elapsed from its own `start`
    /// Issue #647: Flow's own selected pane must freeze a stopped
    /// agent's elapsed at the run's end -- `build_flow_screen` has
    /// its own `build_focused_step` call, which must get
    /// `run_ended_at` too.
    #[test]
    fn flow_screens_stopped_agents_elapsed_freezes_at_the_runs_end_not_osis_wall_clock() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T11:46:43Z", "completed_at": "2026-01-01T11:46:54Z",
                "error": "Interrupted (Ctrl-C/SIGINT)"
            },
            "fix": {"value": null, "meta": {
                "created_at": "2026-01-01T11:46:43Z", "completed_at": null, "provider": "agent",
                "live": {
                    "engine": "pi", "updated_at": "2026-01-01T11:46:50Z",
                    "current_tool": {"name": "bash", "started_at": "2026-01-01T11:46:50Z"}
                }
            }}
        }});
        let flat = flat_from(state.clone());
        let statuses = model.observe(&state, &plan, ProcessState::exited(true));
        let all_paths: BTreeMap<String, ()> = statuses
            .keys()
            .chain(flat.keys())
            .map(|k| (k.clone(), ()))
            .collect();
        let rows = crate::render::build_rows(
            &plan,
            &flat,
            &statuses,
            &all_paths,
            "2026-01-01T11:52:55Z",
            &model,
            None,
        );

        let elapsed_at = |now: &str| {
            let screen = build_flow(
                &rows,
                &flat,
                &plan,
                &model,
                &base_header(),
                Some(&state),
                &[],
                Some("prime.fix"),
                &utc(),
                Some("2026-01-01T11:46:54Z"),
                now,
            );
            screen
                .selected_step
                .expect("prime.fix is selectable")
                .elapsed_s
                .expect("since_ts is known")
        };
        let soon = elapsed_at("2026-01-01T11:46:55Z");
        let long_after = elapsed_at("2026-02-01T00:00:00Z");
        assert!((soon - 11.0).abs() < 1e-9); // 11:46:43 -> 11:46:54
        assert_eq!(
            soon, long_after,
            "a stopped agent's own elapsed must not tick with osi's wall clock"
        );
    }

    /// Issue #647: `running: <tool>` must not survive past the
    /// session itself stopping -- `current_tool_elapsed_s` is `None`
    /// once stale, which is what tells `agent_live_lines` to show
    /// `last: <tool>` instead.
    #[test]
    fn a_stale_leftover_current_tool_gets_no_elapsed() {
        let plan = PlanTree::empty();
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "fix": {"value": null, "meta": {
                    "created_at": "2026-01-01T11:46:43Z", "completed_at": null, "provider": "agent",
                    "live": {
                        "engine": "pi", "updated_at": "2026-01-01T11:46:50Z",
                        "current_tool": {"name": "bash", "started_at": "2026-01-01T11:46:50Z"}
                    }
                }}
            }}),
        );
        let row = Row {
            path: "prime.fix".to_string(),
            depth: 0,
            label: "fix".to_string(),
            kind: RowKind::Tool,
            status: RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: false,
        };
        let model = RunModel::new();
        // Long after the heartbeat -- `is_recent` reads this leftover
        // as stale, the same as a stopped run viewed much later.
        let step = build_focused_step(
            &row,
            &flat,
            &plan,
            &model,
            None,
            "2026-01-01T12:00:00Z",
            None,
        );
        match step.detail {
            FocusDetail::Agent {
                stale,
                current_tool_elapsed_s,
                ..
            } => {
                assert!(stale);
                assert_eq!(current_tool_elapsed_s, None);
            }
            other => panic!("expected Agent, got {other:?}"),
        }
    }

    /// Issue #647: a running tool call's own elapsed time, read from
    /// `meta.live.current_tool.started_at` -- separate from the
    /// session's own total (`FocusedStep::elapsed_s`), which is
    /// `since_ts` to `now` regardless of which tool is running.
    #[test]
    fn a_live_current_tools_own_elapsed_is_computed_from_its_started_at() {
        let plan = PlanTree::empty();
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "fix": {"value": null, "meta": {
                    "created_at": "2026-01-01T11:46:43Z", "completed_at": null, "provider": "agent",
                    "live": {
                        "engine": "pi", "updated_at": "2026-01-01T11:52:54Z",
                        "current_tool": {"name": "bash", "started_at": "2026-01-01T11:52:30Z"}
                    }
                }}
            }}),
        );
        let row = Row {
            path: "prime.fix".to_string(),
            depth: 0,
            label: "fix".to_string(),
            kind: RowKind::Tool,
            status: RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: false,
        };
        let model = RunModel::new();
        let step = build_focused_step(
            &row,
            &flat,
            &plan,
            &model,
            None,
            "2026-01-01T11:52:54Z",
            None,
        );
        match step.detail {
            FocusDetail::Agent {
                stale,
                current_tool_elapsed_s,
                ..
            } => {
                assert!(!stale);
                assert!((current_tool_elapsed_s.unwrap() - 24.0).abs() < 1e-9);
            }
            other => panic!("expected Agent, got {other:?}"),
        }
    }

    /// Issue #635: selecting a decision row on the Flow screen gives
    /// the selected pane the condition/branch detail instead of an
    /// ordinary `FocusedStep` -- `check` is a named `if` here, so it
    /// still has a real row of its own to select by path.
    #[test]
    fn build_flow_screen_selected_decision_reads_the_condition_and_branch() {
        let plan = PlanTree::from_program(&program_with_ifs());
        let state = json!({
            "prime": {
                "value": null, "meta": {"created_at": "2026-10-08T11:00:00Z", "completed_at": null},
                "plan": {"value": "x", "meta": {"created_at": "2026-10-08T11:00:00Z", "completed_at": "2026-10-08T11:00:05Z"}},
                "check": {
                    "value": {"branch": "then"}, "meta": {"created_at": "2026-10-08T11:00:05Z", "completed_at": "2026-10-08T11:00:06Z", "branch": "then"},
                    "taken": {"value": "ok\n", "meta": {"created_at": "2026-10-08T11:00:06Z", "completed_at": "2026-10-08T11:00:06Z"}}
                }
            }
        });
        let flat = flat_from(state.clone());
        let mut model = RunModel::new();
        let statuses = model.observe(&state, &plan, ProcessState::Running);
        let all_paths: BTreeMap<String, ()> = statuses
            .keys()
            .chain(flat.keys())
            .map(|k| (k.clone(), ()))
            .collect();
        let rows = crate::render::build_rows(
            &plan,
            &flat,
            &statuses,
            &all_paths,
            "2026-10-08T11:00:10Z",
            &model,
            None,
        );
        let screen = build_flow(
            &rows,
            &flat,
            &plan,
            &model,
            &base_header(),
            Some(&state),
            &[],
            Some("prime.check"),
            &utc(),
            None,
            "2026-10-08T11:00:10Z",
        );
        assert!(screen.selected_step.is_none());
        let decision = screen.selected_decision.expect("a decision selection");
        assert_eq!(decision.branch.as_deref(), Some("then"));
        assert!(!decision.inferred);
        assert_eq!(
            decision.condition,
            Some(crate::plan::ConditionInfo {
                mode: crate::plan::ConditionMode::Cel,
                text: "true".to_string()
            })
        );
    }

    #[test]
    fn phase_strip_folds_single_child_wrappers_and_counts_depth_one() {
        let plan = PlanTree::from_program(&program_with_ifs());
        let state = json!({
            "prime": {
                "value": null, "meta": {"created_at": "2026-10-08T11:00:00Z", "completed_at": null},
                "plan": {"value": "x", "meta": {"created_at": "2026-10-08T11:00:00Z", "completed_at": "2026-10-08T11:00:05Z"}},
                "check": {
                    "value": {"branch": "then"}, "meta": {"created_at": "2026-10-08T11:00:05Z", "completed_at": "2026-10-08T11:00:06Z", "branch": "then"},
                    "taken": {"value": "ok\n", "meta": {"created_at": "2026-10-08T11:00:06Z", "completed_at": "2026-10-08T11:00:06Z"}}
                }
            }
        });
        let flat = flat_from(state.clone());
        let mut model = RunModel::new();
        let statuses = model.observe(&state, &plan, ProcessState::Running);
        let all_paths: BTreeMap<String, ()> = flat.keys().map(|k| (k.clone(), ())).collect();
        let rows = crate::render::build_rows(
            &plan,
            &flat,
            &statuses,
            &all_paths,
            "2026-10-08T11:00:10Z",
            &model,
            None,
        );
        let folded = crate::fold::fold_rows(rows, true, &Default::default());
        let strip = build_phase_strip(&folded);
        // Issue #635: a named `if` is no longer breadcrumb-eligible, so
        // `check` no longer folds with its own single child `taken`
        // (which drops to depth 2, out of the phase strip entirely) --
        // the strip has two top-level rows, `plan` and `check` on its
        // own, neither carrying a › breadcrumb.
        assert_eq!(strip.len(), 2);
        assert!(strip.iter().any(|s| s.label == "check"));
        assert!(!strip.iter().any(|s| s.label.contains('\u{203a}')));
    }

    /// Issue #641: with more top-level phases than the front end has
    /// distinct glyphs for (`max_phase_bands`), the chart's own
    /// series cap to that many, the smallest ones folded into one
    /// `other` band -- never silently reusing a glyph for two
    /// different phases.
    #[test]
    fn the_chart_caps_its_own_bands_to_max_phase_bands() {
        let header = base_header();
        let mut flat = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        let mut kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let mut rows = Vec::new();
        for i in 0..8 {
            let path = format!("prime.p{i}");
            flat.insert(
                path.clone(),
                NodeMeta {
                    created_at: Some("2026-10-08T11:04:58Z".to_string()),
                    completed_at: Some("2026-10-08T11:05:58Z".to_string()),
                    error: None,
                    branch: None,
                    progress_done: None,
                    progress_total: None,
                    progress_eta_s: None,
                    live: None,
                    prompt_live: None,
                    value: Value::Null,
                    meta: json!({
                        "adapter": "scripted",
                        "tokens_sent_total": (i + 1) * 10,
                        "tokens_received_total": 0
                    }),
                },
            );
            let status = RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            };
            statuses.insert(path.clone(), status.clone());
            kinds_map.insert(path.clone(), RowKind::Prompt);
            rows.push(Row {
                path: path.clone(),
                depth: 1,
                label: format!("p{i}"),
                kind: RowKind::Prompt,
                status,
                duration_s: Some(60.0),
                loop_progress: None,
                dim: None,
                has_children: false,
            });
        }
        let model = RunModel::new();
        let screen = build_cost_screen(
            &rows,
            &header,
            &flat,
            &statuses,
            &kinds_map,
            None,
            crate::treemap::Measure::Total,
            &model,
            "2026-10-08T11:52:55Z",
            6,
        );
        assert_eq!(screen.tokens_over_time.len(), 6);
        assert_eq!(screen.tokens_over_time.last().unwrap().0, "other");
    }

    fn agent_row(path: &str) -> Row {
        Row {
            path: path.to_string(),
            depth: 0,
            label: path.rsplit('.').next().unwrap_or(path).to_string(),
            kind: RowKind::Tool,
            status: RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: false,
        }
    }

    /// Issue #637: a running agent's own `meta.live` fills
    /// `FocusDetail::Agent::live`, replacing the old always-on
    /// fallback note.
    #[test]
    fn a_running_agents_live_summary_fills_focus_detail() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "agent1": {"value": null, "meta": {
                    "created_at": "2026-10-08T11:46:43Z",
                    "completed_at": null,
                    "provider": "agent",
                    "live": {
                        "engine": "pi", "model": "sonnet", "turns": 14, "tool_calls": 31,
                        "tokens_sent": 2100000, "tokens_received": 18000, "cost_usd": 1.10,
                        "updated_at": "2026-10-08T11:52:54Z"
                    }
                }}
            }}),
        );
        let row = agent_row("prime.agent1");
        let step = build_focused_step(
            &row,
            &flat,
            &plan,
            &model,
            None,
            "2026-10-08T11:52:55Z",
            None,
        );
        match step.detail {
            FocusDetail::Agent {
                live: Some(live),
                stale,
                ..
            } => {
                assert_eq!(live.engine, Some("pi".to_string()));
                assert_eq!(live.turns, Some(14));
                assert!(live.is_recent("2026-10-08T11:52:55Z"));
                assert!(!stale);
            }
            other => panic!("expected a populated Agent detail, got {other:?}"),
        }
    }

    /// An interrupted run's own leftover `meta.live` (no
    /// `completed_at`, no further heartbeat) still fills `live` --
    /// the front end, not this builder, decides running vs stopped
    /// from `AgentLive::is_recent` -- but it must read as stale here.
    #[test]
    fn an_interrupted_agents_leftover_live_summary_reads_as_stale() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "agent1": {"value": null, "meta": {
                    "created_at": "2026-10-08T11:46:43Z",
                    "completed_at": null,
                    "provider": "agent",
                    "live": {"engine": "pi", "updated_at": "2026-10-08T11:46:50Z"}
                }}
            }}),
        );
        let row = agent_row("prime.agent1");
        let step = build_focused_step(
            &row,
            &flat,
            &plan,
            &model,
            None,
            "2026-10-08T11:52:55Z",
            None,
        );
        match step.detail {
            FocusDetail::Agent {
                live: Some(live),
                stale,
                ..
            } => {
                assert!(!live.is_recent("2026-10-08T11:52:55Z"));
                assert!(stale);
            }
            other => panic!("expected a populated Agent detail, got {other:?}"),
        }
    }

    /// A finished agent step has no `meta.live` left at all (removed
    /// once the attempt ended) -- `live` is `None`, and the generic
    /// `tokens`/`cost_usd`/`value` fields (shared with every other
    /// tool step), plus `meta.raw`'s own transcript/files_changed/
    /// turns, are what a front end reads instead.
    #[test]
    fn a_finished_agent_step_has_no_live_field_and_carries_raw_fields() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": "t"},
                "agent1": {"value": "done", "meta": {
                    "created_at": "2026-10-08T11:46:43Z",
                    "completed_at": "2026-10-08T11:52:55Z",
                    "provider": "agent",
                    "tokens_sent": 2100000, "tokens_received": 18000, "cost_usd": 1.10,
                    "raw": {
                        "transcript": "/tmp/t.jsonl", "files_changed": 3, "turns": 14,
                        "tool_calls": 5,
                        "tool_counts": {"bash": {"calls": 3, "ms": 2500}, "edit": {"calls": 2, "ms": 900}}
                    }
                }}
            }}),
        );
        let mut row = agent_row("prime.agent1");
        row.status = RowStatus {
            kind: StatusKind::Done,
            skip_reason: None,
            retrying: false,
        };
        let step = build_focused_step(
            &row,
            &flat,
            &plan,
            &model,
            None,
            "2026-10-08T11:52:56Z",
            None,
        );
        match step.detail {
            FocusDetail::Agent {
                live: None,
                note,
                raw_transcript,
                raw_files_changed,
                raw_turns,
                raw_tool_calls,
                raw_tool_counts,
                ..
            } => {
                assert_eq!(note, AGENT_LIVE_DATA_NOTE);
                assert_eq!(raw_tool_calls, Some(5));
                assert_eq!(
                    raw_tool_counts.get("bash"),
                    Some(&crate::model::AgentLiveToolUsage { calls: 3, ms: 2500 })
                );
                assert_eq!(raw_transcript, Some("/tmp/t.jsonl".to_string()));
                assert_eq!(raw_files_changed, Some(3));
                assert_eq!(raw_turns, Some(14));
            }
            other => panic!("expected no live summary, got {other:?}"),
        }
        assert_eq!(step.tokens, Some(2_100_000 + 18_000));
        assert_eq!(step.cost_usd, Some(1.10));
    }

    #[test]
    fn agent_asked_reads_the_first_line_of_a_plain_prompt() {
        let meta =
            json!({"params_rendered": {"prompt": "Fix the bug.\nSee the issue for details."}});
        assert_eq!(agent_asked_text(&meta), Some("Fix the bug.".to_string()));
    }

    #[test]
    fn agent_asked_reads_the_file_path_for_a_prompt_read_from_one() {
        let meta =
            json!({"params_rendered": {"prompt": {"file": "/repo/prompt.md", "within": "/repo"}}});
        assert_eq!(agent_asked_text(&meta), Some("/repo/prompt.md".to_string()));
    }

    /// A retried attempt's own `meta.live.earlier_attempts` survives
    /// into `FocusDetail::Agent::live` unchanged.
    #[test]
    fn a_retried_attempts_earlier_attempts_is_carried_through() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let flat = flat_from(
            json!({"prime": {"value": null, "meta": {"completed_at": null},
                "agent1": {"value": null, "meta": {
                    "created_at": "2026-10-08T11:50:00Z",
                    "completed_at": null,
                    "provider": "agent",
                    "live": {
                        "attempt": 2,
                        "updated_at": "2026-10-08T11:52:54Z",
                        "earlier_attempts": {"tokens_sent": 500, "tokens_received": 40}
                    }
                }}
            }}),
        );
        let row = agent_row("prime.agent1");
        let step = build_focused_step(
            &row,
            &flat,
            &plan,
            &model,
            None,
            "2026-10-08T11:52:55Z",
            None,
        );
        match step.detail {
            FocusDetail::Agent {
                live: Some(live), ..
            } => {
                assert_eq!(live.attempt, Some(2));
                assert_eq!(
                    live.earlier_attempts,
                    Some(crate::model::AgentLiveEarlierAttempts {
                        tokens_sent: Some(500),
                        tokens_received: Some(40)
                    })
                );
            }
            other => panic!("expected a populated Agent detail, got {other:?}"),
        }
    }

    // -------------------------------------------------------------
    // Signal (issue #688)
    // -------------------------------------------------------------

    fn signal_node(
        created_at: &str,
        completed_at: Option<&str>,
        sent: u64,
        received: u64,
    ) -> NodeMeta {
        let mut n = tape_node(created_at, completed_at);
        n.meta = json!({"tokens_sent": sent, "tokens_received": received, "adapter": "scripted", "model": "scripted-model"});
        n
    }

    #[test]
    fn signal_axis_top_with_history_holds_1_5x_the_usual_largest_step() {
        // 1.5 * 390 = 585, the smallest of 300/600/1200/... at or
        // above it is 600 -- the design's own `run2` fixture.
        assert_eq!(signal_axis_top(Some(390), 0, 600), 600);
        // 1.5 * 30 = 45, the smallest of 40/80/160/... at or above
        // it is 80.
        assert_eq!(signal_axis_top(Some(30), 0, 40), 80);
        assert_eq!(signal_axis_top(Some(0), 0, 600), 600);
    }

    #[test]
    fn signal_axis_top_without_history_doubles_once_never_again() {
        assert_eq!(signal_axis_top(None, 0, 600), 600);
        assert_eq!(signal_axis_top(None, 600, 600), 600);
        assert_eq!(signal_axis_top(None, 601, 600), 1200);
        // Never a second doubling, however far over the run goes.
        assert_eq!(signal_axis_top(None, 100_000, 600), 1200);
    }

    /// Three prompts summing well past 600 tokens over the whole
    /// run, no single one past it, never double the axis -- the
    /// no-history rule measures the largest single column, not the
    /// run's own running total.
    #[test]
    fn build_signal_axis_without_history_measures_the_largest_column_not_the_run_total() {
        let rows = vec![
            tape_row("prime.a", 1, "a", RowKind::Prompt, StatusKind::Done),
            tape_row("prime.b", 1, "b", RowKind::Prompt, StatusKind::Done),
            tape_row("prime.c", 1, "c", RowKind::Prompt, StatusKind::Done),
        ];
        let mut flat = BTreeMap::new();
        for (name, end) in [
            ("a", "2026-01-01T00:00:01.000Z"),
            ("b", "2026-01-01T00:00:02.000Z"),
            ("c", "2026-01-01T00:00:03.000Z"),
        ] {
            flat.insert(
                format!("prime.{name}"),
                signal_node("2026-01-01T00:00:00.000Z", Some(end), 300, 20),
            );
        }
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        header.tokens_sent = 900;
        header.tokens_received = 60;
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let signal = build_signal(
            &rows,
            &header,
            &flat,
            &plan,
            &model,
            None,
            100,
            None,
            "2026-01-01T00:00:03.100Z",
        );
        assert_eq!(signal.sent_axis_top, 600, "no single column passed 600");
    }

    /// Two top-level prompts ending in the same 100 ms column sum
    /// into that one column rather than each getting their own.
    #[test]
    fn build_signal_sums_two_prompts_ending_in_the_same_column() {
        let rows = vec![
            tape_row("prime.a", 1, "a", RowKind::Prompt, StatusKind::Done),
            tape_row("prime.b", 1, "b", RowKind::Prompt, StatusKind::Done),
        ];
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.a".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:03.050Z"),
                100,
                10,
            ),
        );
        flat.insert(
            "prime.b".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:03.070Z"),
                50,
                5,
            ),
        );
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        header.tokens_sent = 150;
        header.tokens_received = 15;
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let signal = build_signal(
            &rows,
            &header,
            &flat,
            &plan,
            &model,
            None,
            100,
            None,
            "2026-01-01T00:00:03.100Z",
        );
        // Bucket 30 spans [3.0s, 3.1s) -- both ends land there.
        let bucket = &signal.buckets[30];
        assert_eq!(bucket.sent, 150);
        assert_eq!(bucket.received, 15);
        assert_eq!(bucket.prompt_count, 2);
        assert!(
            !signal.buckets[0..30]
                .iter()
                .any(|b| b.sent > 0 || b.received > 0)
        );
    }

    /// A decision's own mark wins over a finished prompt's `✓` when
    /// both land in the same column, matching `run2`'s own frame:
    /// `◆` at the column three prompts and a decision share.
    #[test]
    fn build_signal_a_decision_mark_wins_over_a_prompt_mark_in_the_same_column() {
        let mut gate = tape_row("prime.gate", 1, "gate", RowKind::If, StatusKind::Done);
        let mut reply = tape_row(
            "prime.gate.reply",
            2,
            "reply",
            RowKind::Prompt,
            StatusKind::Done,
        );
        gate.depth = 1;
        reply.depth = 2;
        let rows = vec![gate, reply];
        let mut flat = BTreeMap::new();
        let mut gate_node = signal_node(
            "2026-01-01T00:00:03.000Z",
            Some("2026-01-01T00:00:03.010Z"),
            0,
            0,
        );
        gate_node.branch = Some("then".to_string());
        flat.insert("prime.gate".to_string(), gate_node);
        flat.insert(
            "prime.gate.reply".to_string(),
            signal_node(
                "2026-01-01T00:00:03.000Z",
                Some("2026-01-01T00:00:03.050Z"),
                10,
                2,
            ),
        );
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let signal = build_signal(
            &rows,
            &header,
            &flat,
            &plan,
            &model,
            None,
            100,
            None,
            "2026-01-01T00:00:03.100Z",
        );
        let mark = signal
            .marks
            .iter()
            .find(|(col, _)| *col == 30)
            .map(|(_, m)| *m);
        assert_eq!(mark, Some(SignalMark::Decision));
    }

    /// A running prompt's own live estimate grows the Now column,
    /// never the column its own start time would otherwise place it
    /// at.
    #[test]
    fn build_signal_a_running_prompts_live_estimate_lands_in_the_now_column() {
        let rows = vec![tape_row(
            "prime.a",
            1,
            "a",
            RowKind::Prompt,
            StatusKind::Running,
        )];
        let mut flat = BTreeMap::new();
        let mut node = tape_node("2026-01-01T00:00:00.000Z", None);
        node.meta = json!({
            "adapter": "scripted", "model": "scripted-model",
            "live": {
                "adapter": "scripted", "attempt": 0, "streaming": true,
                "started_at": "2026-01-01T00:00:00.000Z",
                "tokens_sent": 40, "tokens_received_estimate": 7,
            }
        });
        node.prompt_live = crate::model::PromptLive::from_value(&node.meta["live"]);
        flat.insert("prime.a".to_string(), node);
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let now = "2026-01-01T00:00:00.650Z";
        let signal = build_signal(&rows, &header, &flat, &plan, &model, None, 100, None, now);
        assert_eq!(signal.now_col, Some(6));
        let bucket = &signal.buckets[6];
        assert_eq!(bucket.sent, 40);
        assert_eq!(bucket.received, 7);
        assert!(bucket.received_live);
    }

    /// Readings' own ratio (to one decimal) and peak (the largest
    /// column's own sent figure and clock) -- §2.2's `11.5 : 1` and
    /// `584 t at 12:50:50`.
    #[test]
    fn build_signal_readings_ratio_and_peak() {
        let rows = vec![
            tape_row("prime.a", 1, "a", RowKind::Prompt, StatusKind::Done),
            tape_row("prime.b", 1, "b", RowKind::Prompt, StatusKind::Done),
        ];
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.a".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:01.000Z"),
                100,
                10,
            ),
        );
        flat.insert(
            "prime.b".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:03.000Z"),
                584,
                66,
            ),
        );
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        header.tokens_sent = 684;
        header.tokens_received = 76;
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let signal = build_signal(
            &rows,
            &header,
            &flat,
            &plan,
            &model,
            None,
            100,
            None,
            "2026-01-01T00:00:03.100Z",
        );
        assert_eq!(signal.readings.ratio, Some(9.0));
        let (peak_tokens, peak_ts) = signal.readings.peak.expect("a peak column");
        assert_eq!(peak_tokens, 584);
        assert_eq!(peak_ts, "2026-01-01T00:00:03.000Z");
    }

    /// The Now pane's own lines: a running leaf with its slot, the
    /// active loop's own summary, Last Burst and Next Signal, all
    /// read off the same `rows`/`flat`/buckets the trace itself is.
    #[test]
    fn build_signal_now_panes_lines() {
        let mut drafts = tape_row(
            "prime.drafts",
            1,
            "drafts",
            RowKind::Loop,
            StatusKind::Running,
        );
        drafts.loop_progress = Some(crate::render::LoopProgress {
            done: 1,
            total: Some(3),
            eta_s: None,
        });
        let rows = vec![
            drafts,
            tape_row(
                "prime.drafts.iter_0.draft",
                3,
                "iter_0 › draft",
                RowKind::Prompt,
                StatusKind::Done,
            ),
            tape_row(
                "prime.drafts.iter_1.draft",
                3,
                "iter_1 › draft",
                RowKind::Prompt,
                StatusKind::Running,
            ),
            tape_row(
                "prime.next",
                1,
                "next",
                RowKind::Prompt,
                StatusKind::Pending,
            ),
        ];
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.drafts.iter_0.draft".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:01.000Z"),
                80,
                50,
            ),
        );
        let mut running_node = tape_node("2026-01-01T00:00:01.500Z", None);
        running_node.meta = json!({
            "adapter": "scripted", "model": "scripted-model",
            "live": {
                "adapter": "scripted", "attempt": 0, "streaming": true,
                "started_at": "2026-01-01T00:00:01.500Z",
                "tokens_sent": 40, "tokens_received_estimate": 7,
            }
        });
        running_node.prompt_live = crate::model::PromptLive::from_value(&running_node.meta["live"]);
        flat.insert("prime.drafts.iter_1.draft".to_string(), running_node);
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let now = "2026-01-01T00:00:02.100Z";
        let signal = build_signal(&rows, &header, &flat, &plan, &model, None, 100, None, now);

        assert_eq!(signal.now_pane.running.len(), 1);
        assert_eq!(signal.now_pane.running[0].path, "prime.drafts.iter_1.draft");
        let (loop_label, loop_status) = signal
            .now_pane
            .loop_summary
            .as_ref()
            .expect("an active loop");
        assert_eq!(loop_label, "drafts");
        assert_eq!(loop_status, "1 of 3 passes · 130 t per pass");
        let last_burst = signal.now_pane.last_burst.as_deref().expect("a last burst");
        // The most recent column with any activity at all -- the
        // running pass's own live estimate at Now, not the earlier
        // finished pass, since it is in fact the newer of the two.
        assert!(
            last_burst.contains("↑40") && last_burst.contains("↓7"),
            "{last_burst:?}"
        );
        let next_signal = signal
            .now_pane
            .next_signal
            .as_deref()
            .expect("a next signal");
        assert!(next_signal.starts_with("next"), "{next_signal:?}");
        assert!(next_signal.contains("queued"), "{next_signal:?}");
    }

    /// A running prompt's own live estimate marks its column
    /// `received_live`; once the node finishes, the same column
    /// carries the exact figure and `received_live` clears -- the
    /// two are mutually exclusive, never both counted at once.
    #[test]
    fn build_signal_the_live_estimate_settles_to_the_exact_count_once() {
        let rows_running = vec![tape_row(
            "prime.a",
            1,
            "a",
            RowKind::Prompt,
            StatusKind::Running,
        )];
        let mut flat_running = BTreeMap::new();
        let mut node = tape_node("2026-01-01T00:00:00.000Z", None);
        node.meta = json!({
            "adapter": "scripted", "model": "scripted-model",
            "live": {
                "adapter": "scripted", "attempt": 0, "streaming": true,
                "started_at": "2026-01-01T00:00:00.000Z",
                "tokens_sent": 40, "tokens_received_estimate": 7,
            }
        });
        node.prompt_live = crate::model::PromptLive::from_value(&node.meta["live"]);
        flat_running.insert("prime.a".to_string(), node);
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let now = "2026-01-01T00:00:00.650Z";
        let running = build_signal(
            &rows_running,
            &header,
            &flat_running,
            &plan,
            &model,
            None,
            100,
            None,
            now,
        );
        assert!(running.buckets[6].received_live);
        assert_eq!(running.buckets[6].received, 7);

        let rows_done = vec![tape_row(
            "prime.a",
            1,
            "a",
            RowKind::Prompt,
            StatusKind::Done,
        )];
        let mut flat_done = BTreeMap::new();
        flat_done.insert(
            "prime.a".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:00.650Z"),
                40,
                9,
            ),
        );
        let done = build_signal(
            &rows_done, &header, &flat_done, &plan, &model, None, 100, None, now,
        );
        assert!(!done.buckets[6].received_live);
        assert_eq!(done.buckets[6].received, 9);
    }

    /// Zooming changes which 100 ms span a bucket's own column
    /// covers -- the same event lands in a different column at 200
    /// ms per column than it does at the 100 ms default.
    #[test]
    fn build_signal_zoom_changes_which_column_an_event_lands_in() {
        let rows = vec![tape_row(
            "prime.a",
            1,
            "a",
            RowKind::Prompt,
            StatusKind::Done,
        )];
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.a".to_string(),
            signal_node(
                "2026-01-01T00:00:00.000Z",
                Some("2026-01-01T00:00:00.350Z"),
                10,
                2,
            ),
        );
        let mut header = base_header();
        header.run_start_ts = Some("2026-01-01T00:00:00Z".to_string());
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let now = "2026-01-01T00:00:00.400Z";
        let at_100ms = build_signal(&rows, &header, &flat, &plan, &model, None, 100, None, now);
        let at_200ms = build_signal(&rows, &header, &flat, &plan, &model, None, 200, None, now);
        assert_eq!(at_100ms.buckets[3].sent, 10, "0.35s is column 3 at 100ms");
        assert_eq!(at_200ms.buckets[1].sent, 10, "0.35s is column 1 at 200ms");
    }
}
