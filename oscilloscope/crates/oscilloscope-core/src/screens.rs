//! Screens (issue #626): `Screen` names the four screens `osi`
//! switches between with `0`-`3`; [`header_line`] builds the one
//! line every screen shares (replacing the old three-row header);
//! [`Overview`] is screen 0's own model, built from exactly the
//! same inputs `render::build` already has (the live state,
//! `--events`, the plan and `runtime.last_run.totals` -- no engine
//! change). Neither function draws anything -- no terminal code
//! lives here (DESIGN.md §6.1), so `oscilloscope`'s `tui.rs` is the
//! only thing that turns a segment's role or a card's field into
//! colour and position.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::fold::is_quick_step;
use crate::format::{ClockZone, format_clock_time};
use crate::model::{NodeMeta, RowStatus, RunModel, RunStatus, StatusKind, run_totals};
use crate::observe::duration_seconds;
use crate::plan::PlanTree;
use crate::render::{Row, RowKind, TokenBar, TokenBlock, details_for, token_bar_in_flight_line};

/// Which of the four screens is on display (issue #650: all four
/// now have a screen to show).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Overview,
    Time,
    Flow,
    Cost,
}

impl Screen {
    pub fn number(self) -> u8 {
        match self {
            Screen::Overview => 0,
            Screen::Time => 1,
            Screen::Flow => 2,
            Screen::Cost => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Screen::Overview => "overview",
            Screen::Time => "time",
            Screen::Flow => "flow",
            Screen::Cost => "cost",
        }
    }

    pub fn is_available(self) -> bool {
        matches!(
            self,
            Screen::Overview | Screen::Time | Screen::Flow | Screen::Cost
        )
    }

    /// The screen a `0`-`3` key names, or `None` for any other key,
    /// or for a digit whose screen isn't built yet (`1`) --
    /// callers never need a second check of [`Screen::is_available`].
    pub fn from_key(c: char) -> Option<Screen> {
        let screen = match c {
            '0' => Screen::Overview,
            '1' => Screen::Time,
            '2' => Screen::Flow,
            '3' => Screen::Cost,
            _ => return None,
        };
        screen.is_available().then_some(screen)
    }

    /// Every screen with a tab, in tab order (issue #626's header).
    pub fn available() -> &'static [Screen] {
        &[Screen::Overview, Screen::Time, Screen::Flow, Screen::Cost]
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
        /// `meta.raw.tool_calls` -- the Time screen's zoom pane footer
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

#[derive(Debug, Clone, PartialEq)]
pub struct EndedSummary {
    pub outcome: RunStatus,
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub cost_usd: Option<f64>,
    /// The agent node still holding `meta.live` with no `completed_at`
    /// once the whole run has ended (issue #637: an interrupted run's
    /// agent reads `stopped`, not silently dropped) -- `None` for an
    /// ordinary finished or failed run with no session left mid-way.
    pub stopped_agent: Option<FocusedStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NowFocus {
    /// Something is running: the focused step, plus how many other
    /// rows are running at the same time (DESIGN.md's "+2 more
    /// running").
    Running(FocusedStep, usize),
    /// Nothing is running, and the run hasn't ended.
    LastFinished(FocusedStep),
    Ended(EndedSummary),
    /// No row has ever appeared at all (a run just starting).
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowPane {
    pub log_lines: Vec<String>,
    pub focus: NowFocus,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TimeCard {
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub elapsed_s: f64,
    pub eta_ts: Option<String>,
    /// Set exactly when `eta_ts` came from history rather than the
    /// loop ETA fallback — the only case the overview's own `(12m)`/
    /// `usual …`/`vs usual …` figures apply to.
    pub eta_from_history: bool,
    /// `eta_ts`'s own seconds-from-now — the overview's own `(12m)`.
    pub eta_remaining_s: Option<f64>,
    /// Set once the run is past its own usual (p90) end — the
    /// overview reads `ETA ?` rather than falling back to the loop
    /// ETA, same three-way choice the header line makes.
    pub eta_past_usual: bool,
    pub usual_low_ts: Option<String>,
    pub usual_high_ts: Option<String>,
    pub vs_usual_s: Option<f64>,
    pub usual_total_s: Option<f64>,
    /// How many ok runs of the current document hash history has,
    /// shown as `history: N of 3 runs` only while it's below the
    /// threshold a whole-run ETA itself needs (`eta_from_history` is
    /// false but history isn't simply off).
    pub history_count: Option<usize>,
    pub bar: Vec<(String, f64)>,
    pub slowest_finished: Option<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlowEvent {
    Decision {
        name: String,
        branch: String,
        ts: Option<String>,
    },
    Report {
        message: String,
        ts: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlowCard {
    pub effects_done: u64,
    pub effects_planned: Option<u64>,
    pub effects_planned_is_lower_bound: bool,
    pub failures: u64,
    /// The latest two decisions or reports, newest first (DESIGN.md:
    /// "the latest two decisions ... or reports").
    pub recent: Vec<FlowEvent>,
    pub loops: Vec<(String, u64, Option<u64>)>,
    pub next_pending: Option<String>,
    pub quick_steps: Option<(u64, f64)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CostCard {
    pub tokens_sent: u64,
    pub tokens_received: u64,
    /// `None` until some effect's own `meta.cost_usd` (or the run's
    /// own `runtime.last_run.totals.cost_usd`) has actually reported
    /// one (DESIGN.md: "cost ... once any effect reports
    /// meta.cost_usd") -- never shown as `$0.00` in the meantime.
    pub cost_usd: Option<f64>,
    /// Tokens (sent + received) completed in each of a fixed number
    /// of equal time buckets spanning the run so far, oldest first --
    /// the sparkline's own data (DESIGN.md: "tokens over time as a
    /// sparkline"), bucketed from exactly what every other card
    /// already reads: each effect's own `meta.tokens_sent`/
    /// `tokens_received` against its own `completed_at`. Empty before
    /// the run has a start time or anything has completed.
    pub tokens_over_time: Vec<u64>,
    pub top_store: Option<(String, u64)>,
    pub in_flight: String,
    /// The ended run's own stopped agent (issue #637) -- its label,
    /// and its last-known `meta.live.tokens_sent`/`tokens_received`
    /// (either may be `None`, same as the live summary itself) --
    /// since `tokens_sent`/`tokens_received` above never include a
    /// session that never completed.
    pub stopped_agent: Option<(String, Option<u64>, Option<u64>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    pub phase_strip: Vec<PhaseStep>,
    pub now_pane: NowPane,
    pub time_card: TimeCard,
    pub flow_card: FlowCard,
    pub cost_card: CostCard,
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
fn stopped_agent_row<'a>(
    rows: &'a [Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
) -> Option<&'a Row> {
    rows.iter().find(|r| {
        is_agent_row(r, flat, plan)
            && node_for(r, flat).is_some_and(|n| n.live.is_some() && n.completed_at.is_none())
    })
}

fn build_focused_step(
    row: &Row,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    now: &str,
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
    // finished step's duration by however long it's been since.
    let elapsed_s =
        since_ts
            .as_deref()
            .and_then(|start| match node.and_then(|n| n.completed_at.as_deref()) {
                Some(end) => duration_seconds(start, end),
                None => duration_seconds(start, now),
            });
    let details = details_for(&row.path, plan, model, state);
    let agent = is_agent_row(row, flat, plan);
    let detail = if agent {
        let live = node.and_then(|n| n.live.clone()).map(Box::new);
        let stale = live.as_deref().is_some_and(|l| !l.is_recent(now));
        let raw = node.map(|n| agent_raw_summary(&n.meta)).unwrap_or_default();
        FocusDetail::Agent {
            asked: node.and_then(|n| agent_asked_text(&n.meta)),
            live,
            stale,
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

#[allow(clippy::too_many_arguments)]
fn build_now_pane(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    header: &crate::render::Header,
    log: &[String],
    selected_path: Option<&str>,
    now: &str,
) -> NowPane {
    const LOG_TAIL: usize = 20;
    let log_lines: Vec<String> = log.iter().rev().take(LOG_TAIL).rev().cloned().collect();

    let focus = if !matches!(header.status, RunStatus::Running) {
        let stopped_agent = stopped_agent_row(rows, flat, plan)
            .map(|row| build_focused_step(row, flat, plan, model, state, now));
        NowFocus::Ended(EndedSummary {
            outcome: header.status,
            start_ts: header.run_start_ts.clone(),
            end_ts: header.run_end_ts.clone(),
            tokens_sent: header.tokens_sent,
            tokens_received: header.tokens_received,
            cost_usd: effective_cost_usd(state, flat),
            stopped_agent,
        })
    } else {
        // The document root is "running" for the whole run, which
        // would otherwise inflate "+N more running" by one for the
        // entire run's duration even with nothing else going on --
        // it's never a real step to surface here.
        //
        // A container counts as running the moment any descendant of
        // its own is (`completed_at` stays unset until every child
        // has), so a leaf actually running makes every one of its
        // ancestors "running" too -- only the leaves (no running
        // descendant of their own) are real steps to count or focus
        // on; `review > fix > agent` with `agent` running must read
        // one running step, not three.
        let all_running: Vec<&Row> = rows
            .iter()
            .filter(|r| r.status.kind == StatusKind::Running && r.kind != RowKind::Root)
            .collect();
        let running: Vec<&Row> = all_running
            .iter()
            .filter(|r| {
                let prefix = format!("{}.", r.path);
                !all_running
                    .iter()
                    .any(|other| other.path.starts_with(&prefix))
            })
            .copied()
            .collect();
        // The step follow selects (DESIGN.md): whichever running row
        // is the shared selection, same as the orchestration pane's
        // own follow target -- falling back to the deepest running
        // row (the same tie-break `keys::App::follow_target` uses)
        // when the selection isn't actually one of the running rows
        // right now (follow off, or mid-transition between rebuilds).
        let target = selected_path
            .and_then(|p| running.iter().find(|r| r.path == p).copied())
            .or_else(|| running.iter().max_by_key(|r| r.depth).copied());
        if let Some(target) = target {
            let mut step = build_focused_step(target, flat, plan, model, state, now);
            if let Some(usual) = crate::history::display_key(&target.path, plan)
                .and_then(|key| header.history_step_usual.get(&key))
            {
                step.usual_busy_p50_s = usual.busy_p50_s;
                step.usual_busy_p90_s = usual.busy_p90_s;
            }
            NowFocus::Running(step, running.len() - 1)
        } else {
            let last_finished = rows
                .iter()
                .filter(|r| r.status.kind == StatusKind::Done)
                .max_by_key(|r| {
                    node_for(r, flat)
                        .and_then(|n| n.completed_at.clone())
                        .unwrap_or_default()
                });
            match last_finished {
                Some(row) => {
                    NowFocus::LastFinished(build_focused_step(row, flat, plan, model, state, now))
                }
                None => NowFocus::None,
            }
        }
    };

    NowPane { log_lines, focus }
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

fn fill_phase_start_times(steps: &mut [PhaseStep], flat: &BTreeMap<String, NodeMeta>) {
    for step in steps.iter_mut() {
        step.start_ts = flat.get(&step.path).and_then(|n| n.created_at.clone());
    }
}

fn build_time_card(header: &crate::render::Header, phase_strip: &[PhaseStep]) -> TimeCard {
    let bar = phase_strip
        .iter()
        .filter_map(|s| s.duration_s.map(|d| (s.label.clone(), d)))
        .collect();
    let slowest_finished = phase_strip
        .iter()
        .filter(|s| s.status.kind == StatusKind::Done)
        .filter_map(|s| s.duration_s.map(|d| (s.label.clone(), d)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    // Issue #649: the same three-way choice the header line makes --
    // `ETA ?` once past the usual end, else history's own `ETA ~`,
    // else the loop ETA -- so the card and the header never disagree.
    let (eta_ts, eta_from_history) = if header.history_eta_past_usual {
        (None, true)
    } else if header.history_eta_ts.is_some() {
        (header.history_eta_ts.clone(), true)
    } else {
        (header.innermost_loop_eta_ts.clone(), false)
    };
    let history_count = if eta_from_history {
        None
    } else {
        header.history_same_hash_count
    };
    TimeCard {
        start_ts: header.run_start_ts.clone(),
        end_ts: header.run_end_ts.clone(),
        elapsed_s: header.elapsed_s,
        eta_ts,
        eta_from_history,
        eta_remaining_s: if eta_from_history {
            header.history_eta_remaining_s
        } else {
            None
        },
        eta_past_usual: header.history_eta_past_usual,
        usual_low_ts: if eta_from_history {
            header.history_eta_low_ts.clone()
        } else {
            None
        },
        usual_high_ts: if eta_from_history {
            header.history_eta_high_ts.clone()
        } else {
            None
        },
        vs_usual_s: if eta_from_history {
            header.history_vs_usual_s
        } else {
            None
        },
        usual_total_s: if eta_from_history {
            header.history_usual_total_s
        } else {
            None
        },
        history_count,
        bar,
        slowest_finished,
    }
}

fn build_flow_card(
    rows: &[Row],
    unfolded_rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    header: &crate::render::Header,
) -> FlowCard {
    // A container's own status propagates from a failed descendant
    // the same way "running" does -- `review > fix > agent` with
    // `agent` failed reads every one of its ancestors as `Failed`/
    // `FailedHandled` too, so only the leaves (no failed descendant
    // of their own) are real failures to count; the root is never
    // one either.
    let failing: Vec<&Row> = rows
        .iter()
        .filter(|r| {
            r.kind != RowKind::Root
                && matches!(
                    r.status.kind,
                    StatusKind::Failed | StatusKind::FailedHandled
                )
        })
        .collect();
    let failures = failing
        .iter()
        .filter(|r| {
            let prefix = format!("{}.", r.path);
            !failing.iter().any(|other| other.path.starts_with(&prefix))
        })
        .count() as u64;

    let mut decisions: Vec<(String, String, Option<String>)> = flat
        .iter()
        .filter_map(|(path, node)| {
            let branch = node.branch.as_ref()?;
            let name = path.rsplit('.').next().unwrap_or(path).to_string();
            let ts = node
                .completed_at
                .clone()
                .or_else(|| node.created_at.clone());
            Some((name, branch.clone(), ts))
        })
        .collect();
    decisions.sort_by(|a, b| b.2.cmp(&a.2));

    let mut recent: Vec<FlowEvent> = decisions
        .into_iter()
        .take(2)
        .map(|(name, branch, ts)| FlowEvent::Decision { name, branch, ts })
        .collect();
    if recent.len() < 2 {
        if let Some(report) = model.latest_report() {
            recent.push(FlowEvent::Report {
                message: report.message.clone(),
                ts: Some(report.ts.clone()),
            });
        }
    }

    let loops: Vec<(String, u64, Option<u64>)> = rows
        .iter()
        .filter(|r| r.kind == RowKind::Loop)
        .filter_map(|r| r.loop_progress.map(|p| (r.label.clone(), p.done, p.total)))
        .collect();

    let next_pending = rows
        .iter()
        .find(|r| r.status.kind == StatusKind::Pending)
        .map(|r| r.label.clone());

    let (count, total_s) = unfolded_rows
        .iter()
        .filter(|r| is_quick_step(r))
        .fold((0u64, 0.0f64), |(c, s), r| {
            (c + 1, s + r.duration_s.unwrap_or(0.0))
        });
    let quick_steps = if count > 0 {
        Some((count, total_s))
    } else {
        None
    };

    FlowCard {
        effects_done: header.effects_done,
        effects_planned: header.effects_planned,
        effects_planned_is_lower_bound: header.effects_planned_is_lower_bound,
        failures,
        recent,
        loops,
        next_pending,
        quick_steps,
    }
}

/// Issue #626: the cost card (and the ended summary, in
/// `build_now_pane`) show a dollar figure the moment any effect's
/// own `meta.cost_usd` has reported one, not only once the run has
/// ended and `runtime.last_run.totals.cost_usd` exists (DESIGN.md:
/// "cost ... once any effect reports meta.cost_usd") -- the
/// authoritative total once cof has written it, summed from every
/// node's own report otherwise. Shown only when it's actually
/// positive -- `$0.00` means "not reported", same as `None`.
fn effective_cost_usd(state: Option<&Value>, flat: &BTreeMap<String, NodeMeta>) -> Option<f64> {
    state
        .and_then(run_totals)
        .and_then(|t| t.get("cost_usd").and_then(Value::as_f64))
        .or_else(|| {
            let sum: f64 = flat
                .values()
                .filter_map(|n| n.meta.get("cost_usd").and_then(Value::as_f64))
                .sum();
            (sum > 0.0).then_some(sum)
        })
        .filter(|c| *c > 0.0)
}

/// Issue #626's own sparkline: tokens completed in each of a fixed
/// number of equal time buckets spanning `run_start_ts`..`now`, from
/// every node's own `meta.tokens_sent`/`tokens_received` against its
/// own `completed_at` -- the same per-node token rule `node_tokens`
/// already applies elsewhere, bucketed by time instead of summed.
fn build_tokens_over_time(
    header: &crate::render::Header,
    flat: &BTreeMap<String, NodeMeta>,
    now: &str,
) -> Vec<u64> {
    const BUCKETS: usize = 12;
    let Some(start) = header
        .run_start_ts
        .as_deref()
        .and_then(crate::diff::parse_instant)
    else {
        return Vec::new();
    };
    let Some(end) = crate::diff::parse_instant(now) else {
        return Vec::new();
    };
    let span_ms = (end - start).num_milliseconds().max(1) as f64;
    let mut buckets = vec![0u64; BUCKETS];
    for node in flat.values() {
        let Some(completed) = node
            .completed_at
            .as_deref()
            .and_then(crate::diff::parse_instant)
        else {
            continue;
        };
        let (sent, received) = crate::model::node_tokens(&node.meta);
        let total = sent + received;
        if total == 0 {
            continue;
        }
        let frac = ((completed - start).num_milliseconds().max(0) as f64 / span_ms).min(0.999_999);
        let idx = ((frac * BUCKETS as f64) as usize).min(BUCKETS - 1);
        buckets[idx] += total;
    }
    buckets
}

fn build_cost_card(
    rows: &[Row],
    header: &crate::render::Header,
    state: Option<&Value>,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    token_bar: &TokenBar,
    now: &str,
) -> CostCard {
    // Issue #637: `header.tokens_sent`/`received` (`runtime.last_run.
    // totals`, or the subtree fallback) never carry a session that
    // never completed -- its own `meta.live` is the only place its
    // last-known figures ever existed, so the stopped agent's own
    // tokens are reported separately here rather than folded in.
    let stopped_agent = (!matches!(header.status, RunStatus::Running))
        .then(|| stopped_agent_row(rows, flat, plan))
        .flatten()
        .and_then(|row| {
            let live = node_for(row, flat)?.live.as_ref()?;
            Some((row.label.clone(), live.tokens_sent, live.tokens_received))
        });
    CostCard {
        tokens_sent: header.tokens_sent,
        tokens_received: header.tokens_received,
        cost_usd: effective_cost_usd(state, flat),
        tokens_over_time: build_tokens_over_time(header, flat, now),
        // Issue #632: the overview's own "largest store" must agree
        // with the Cost screen's own ranking -- `crate::cost::
        // top_store` is the same leaf-scan rule that screen's own
        // table uses, read directly from `flat` rather than through
        // the Cost screen's own by-store tree, which (DESIGN.md's
        // performance rule) is built only while that screen is
        // actually on display.
        top_store: crate::cost::top_store(flat, "prime"),
        in_flight: token_bar_in_flight_line(token_bar),
        stopped_agent,
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
    now: &str,
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
    let tokens_over_time =
        crate::cost::tokens_over_time_by_phase(header.run_start_ts.as_deref(), end, flat, &phases);
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

/// Builds screen 0's own model (issue #626) -- a caller builds this
/// only while the overview is actually on display (performance:
/// "models for screens not on display aren't built"), right after
/// `render::build_folded`, from exactly the pieces that call already
/// produced: `rows` is the already-folded orchestration rows (so the
/// phase strip's own single-child folding is `fold::fold_rows`'s,
/// not reimplemented here), `unfolded_rows` is `RenderState::
/// unfolded_rows` (quick steps are counted from the real tree, not
/// however folding happened to group them this frame).
#[allow(clippy::too_many_arguments)]
pub fn build_overview(
    rows: &[Row],
    unfolded_rows: &[Row],
    header: &crate::render::Header,
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    token_bar: &TokenBar,
    log: &[String],
    selected_path: Option<&str>,
    now: &str,
) -> Overview {
    let mut phase_strip = build_phase_strip(rows);
    fill_phase_start_times(&mut phase_strip, flat);
    let now_pane = build_now_pane(
        rows,
        flat,
        plan,
        model,
        state,
        header,
        log,
        selected_path,
        now,
    );
    let time_card = build_time_card(header, &phase_strip);
    let flow_card = build_flow_card(rows, unfolded_rows, flat, model, header);
    let cost_card = build_cost_card(rows, header, state, flat, plan, token_bar, now);
    Overview {
        phase_strip,
        now_pane,
        time_card,
        flow_card,
        cost_card,
    }
}

/// A decision row's own detail, for the selected pane when the
/// flow pane's own selection is a `◆`/untaken-branch row rather than
/// an ordinary step (issue #635: "a decision row shows its condition,
/// its mode, the branch it took, and the model's answer for a
/// model-mode condition").
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedDecision {
    pub label: String,
    pub condition: Option<crate::plan::ConditionInfo>,
    pub branch: Option<String>,
    pub inferred: bool,
    pub nothing_ran: bool,
    pub answer: Option<String>,
}

/// Screen 2's own model (issue #635, part of #612) -- built only
/// while the Flow screen is actually on display, the same
/// performance rule `build_cost_screen` already follows for screen
/// 3. `rows`/`footer`/`story` are `crate::flow::build_flow_rows`'s
/// and `build_story`'s own outputs; `selected`/`selected_decision`
/// pick whichever of the two the current selection actually is --
/// never both, since a flow-pane selection is always exactly one
/// kind of row at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowScreen {
    pub rows: Vec<crate::flow::FlowRow>,
    pub footer: crate::flow::FlowFooter,
    pub story: Vec<crate::flow::StoryEntry>,
    pub selected: Option<FocusedStep>,
    pub selected_decision: Option<SelectedDecision>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_flow_screen(
    rows: &[Row],
    unfolded_rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    state: Option<&Value>,
    story_lines: &[crate::diff::LogLine],
    selected_path: Option<&str>,
    zone: &dyn ClockZone,
    now: &str,
) -> FlowScreen {
    let (flow_rows, footer) =
        crate::flow::build_flow_rows(rows, unfolded_rows, flat, plan, story_lines);
    let story = crate::flow::build_story(story_lines, &flow_rows, zone);
    let mut selected = None;
    let mut selected_decision = None;
    if let Some(path) = selected_path {
        // A named `if`'s own decision row shares its real `Row`'s own
        // path, so `flow_rows` is checked first -- `rows` is only the
        // fallback for an ordinary step, which never has a `Decision`
        // entry of its own in `flow_rows` at all.
        let flow_row = flow_rows.iter().find(|r| r.path == path);
        match flow_row.map(|r| &r.kind) {
            Some(crate::flow::FlowRowKind::Decision {
                branch,
                inferred,
                nothing_ran,
                answer,
            }) => {
                selected_decision = Some(SelectedDecision {
                    label: flow_row.unwrap().label.clone(),
                    condition: flow_row.unwrap().condition.clone(),
                    branch: branch.clone(),
                    inferred: *inferred,
                    nothing_ran: *nothing_ran,
                    answer: answer.clone(),
                });
            }
            _ => {
                if let Some(row) = rows.iter().find(|r| r.path == path) {
                    selected = Some(build_focused_step(row, flat, plan, model, state, now));
                }
            }
        }
    }
    FlowScreen {
        rows: flow_rows,
        footer,
        story,
        selected,
        selected_decision,
    }
}

/// A loop row's own pass rows, spliced into [`TimeScreen::rows`]
/// right after it when its path is in `expanded_loops` (DESIGN.md:
/// "`⏎` on a loop row expands it into one row per pass") -- `label`
/// already carries [`crate::time::LoopPassRow::label`]'s own prefix
/// (`pass 1`/`#1`), since a pass row has no real tree path of its
/// own to show a breadcrumb for.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeLoopPassRow {
    pub label: String,
    pub status: RowStatus,
    pub start_s: Option<f64>,
    pub duration_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimeDisplayRow {
    Row(crate::time::TimeRow),
    Pass(TimeLoopPassRow),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeStrips {
    /// Unbucketed (DESIGN.md's events/min strip): `oscilloscope`'s
    /// `tui.rs` buckets these at whatever width and visible window
    /// the bars themselves are drawn at right now, so the two always
    /// line up (issue #650) -- a separate, fixed resolution here
    /// couldn't keep up with `+`/`-`'s own axis zoom.
    pub events_points: Vec<(f64, u64)>,
    pub events_from_state: bool,
    pub decisions: Vec<f64>,
    /// Every currently-in-view leaf row's own `[start_s, end_s]`
    /// (DESIGN.md's in-flight strip), a running leaf's own end
    /// extended to now -- a container row never counts here, or one
    /// running leaf inside a chain of containers would read as two
    /// or three "in flight" instead of one.
    pub in_flight_intervals: Vec<(f64, f64)>,
    pub queued: u64,
    pub quiet_agent: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeVsUsualRow {
    /// The timeline's own row path -- a front end filters against
    /// its own currently *visible* rows with this (DESIGN.md: vs
    /// usual "lists only the timeline's own rows", a collapsed or
    /// filtered-out row included), since `build_vs_usual_pane` itself
    /// has no view of a collapse or filter state of its own.
    pub path: String,
    pub label: String,
    pub now_s: f64,
    pub running: bool,
    pub p50_s: f64,
    pub mark: crate::time::VsUsualMark,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TimeVsUsualPane {
    pub rows: Vec<TimeVsUsualRow>,
    pub earlier_versions: bool,
    /// `Some((count, threshold))` below the run-count threshold
    /// (DESIGN.md: "`history: 1 of 3 runs of this version`") --
    /// `rows` is always empty in this case.
    pub too_few_runs: Option<(usize, usize)>,
    pub finish_ts: Option<String>,
    pub usual_low_ts: Option<String>,
    pub usual_high_ts: Option<String>,
    /// The current hash's own ok-run count normally; under "earlier
    /// versions" (`earlier_versions`), the largest pooled count among
    /// the rows actually shown instead -- never the current hash's
    /// count there, which is always below the threshold by
    /// definition and would read `from 0 runs` for figures pooled
    /// from other versions entirely. `None` when there is nothing
    /// meaningful to show.
    pub from_runs: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeScreen {
    pub rows: Vec<TimeDisplayRow>,
    pub axis: crate::time::AxisSpan,
    /// `now`'s own run-relative offset, for the now line -- `None`
    /// exactly when `axis.show_now` is `false` or the run has no
    /// start yet.
    pub now_s: Option<f64>,
    pub planned: Option<crate::time::GhostBar>,
    pub strips: TimeStrips,
    pub selected: Option<FocusedStep>,
    /// The zoom pane's own extra data (DESIGN.md's zoom pane) for a
    /// container, a prompt or a tool -- `None` for an agent (whose
    /// own zoom draws straight from [`Self::selected`]'s
    /// `FocusDetail::Agent`) or for nothing selected at all.
    pub zoom_extra: Option<TimeZoomExtra>,
    pub vs_usual: TimeVsUsualPane,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimeZoomExtra {
    /// DESIGN.md's zoom pane: "A container: its own children as rows
    /// with bars" -- direct children only, on the container's own
    /// axis (`axis_end_s`, from its own start, DESIGN.md: "on its own
    /// axis from its start to its end (or now)").
    Container {
        children: Vec<crate::time::TimeRow>,
        start_s: f64,
        axis_end_s: f64,
    },
    /// DESIGN.md's zoom pane: "A prompt or tool: its own bar, with
    /// its p50 and p90 marked on the axis when history has them, and
    /// `waiting for the reply` or `running`".
    StepBar {
        start_s: f64,
        p50_s: Option<f64>,
        p90_s: Option<f64>,
        axis_end_s: f64,
        /// `Some("waiting for the reply")` for a running prompt,
        /// `Some("running")` for a running tool, `None` once it has
        /// finished.
        waiting_label: Option<&'static str>,
    },
}

/// [`TimeZoomExtra`], for `selected_path`'s own row -- `None` for an
/// agent (`FocusDetail::Agent` already has everything its own zoom
/// needs) or any row with neither children nor a leaf kind.
fn build_time_zoom_extra(
    selected_path: &str,
    display_rows: &[TimeDisplayRow],
    plan: &PlanTree,
    header: &crate::render::Header,
    now_s: Option<f64>,
) -> Option<TimeZoomExtra> {
    let selected = display_rows.iter().find_map(|r| match r {
        TimeDisplayRow::Row(tr) if tr.path == selected_path => Some(tr),
        _ => None,
    })?;
    let start_s = selected.start_s?;
    let axis_end_s = start_s
        + selected
            .duration_s
            .unwrap_or_else(|| now_s.map_or(0.0, |n| (n - start_s).max(0.0)));
    if selected.has_children {
        let children: Vec<crate::time::TimeRow> = display_rows
            .iter()
            .filter_map(|r| match r {
                TimeDisplayRow::Row(tr)
                    if tr.depth == selected.depth + 1
                        && tr.path.starts_with(&format!("{selected_path}.")) =>
                {
                    Some(tr.clone())
                }
                _ => None,
            })
            .collect();
        return Some(TimeZoomExtra::Container {
            children,
            start_s,
            axis_end_s,
        });
    }
    if matches!(selected.kind, RowKind::Tool | RowKind::Prompt) {
        let key = crate::history::display_key(selected_path, plan);
        let stats = key
            .as_deref()
            .and_then(|k| header.history_step_usual.get(k));
        let waiting_label = (selected.status.kind == StatusKind::Running).then(|| {
            if selected.kind == RowKind::Prompt {
                "waiting for the reply"
            } else {
                "running"
            }
        });
        return Some(TimeZoomExtra::StepBar {
            start_s,
            p50_s: stats.and_then(|s| s.busy_p50_s),
            p90_s: stats.and_then(|s| s.busy_p90_s),
            axis_end_s,
            waiting_label,
        });
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub fn build_time_screen(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    header: &crate::render::Header,
    state: Option<&Value>,
    story_lines: &[crate::diff::LogLine],
    selected_path: Option<&str>,
    expanded_loops: &std::collections::BTreeSet<String>,
    now: &str,
) -> TimeScreen {
    let time_rows =
        crate::time::build_time_rows(rows, flat, model, plan, header, &header.history_step_usual);
    let axis = crate::time::axis_span(header, now);
    let run_start_ts = header.run_start_ts.clone();
    let now_s = if axis.show_now {
        run_start_ts
            .as_deref()
            .and_then(|start| duration_seconds(start, now))
    } else {
        None
    };
    let planned = now_s
        .and_then(|offset| crate::time::planned_ghost_bar(offset, header.history_eta_remaining_s));

    let mut display_rows = Vec::with_capacity(time_rows.len());
    for row in time_rows {
        // An unnamed loop's body shares its row with the leaf
        // underneath it (`RowKind::Tool`/`Prompt`, never
        // `RowKind::Loop` of its own), so `row.loop_expandable` alone
        // -- already true only for a real loop container or an
        // actually-expandable unnamed loop body -- decides this.
        let expand = row.loop_expandable && expanded_loops.contains(&row.path);
        let path = row.path.clone();
        display_rows.push(TimeDisplayRow::Row(row));
        if expand {
            if let Some(run_start_ts) = run_start_ts.as_deref() {
                for pass in crate::time::expand_loop(&path, flat, model, run_start_ts, now) {
                    display_rows.push(TimeDisplayRow::Pass(TimeLoopPassRow {
                        label: pass.label,
                        status: pass.status,
                        start_s: pass.start_s,
                        duration_s: pass.duration_s,
                    }));
                }
            }
        }
    }

    let strips = run_start_ts.as_deref().map_or_else(
        || TimeStrips {
            events_points: Vec::new(),
            events_from_state: !model.has_events(),
            decisions: Vec::new(),
            in_flight_intervals: Vec::new(),
            queued: crate::time::queued_now(flat),
            quiet_agent: false,
        },
        |run_start_ts| {
            // Leaf rows only, and only ones without child rows of
            // their own (`RowKind::is_leaf` alone still includes a
            // `use`, which can have children) -- a container counting
            // alongside the one leaf actually running inside it would
            // read "in flight" as 2 or 3 for a single running step.
            let leaf_intervals: Vec<(f64, f64, bool, StatusKind)> = rows
                .iter()
                .filter(|r| r.kind.is_leaf() && !r.has_children)
                .flat_map(|r| {
                    // An unnamed loop's body leaf: named and unnamed
                    // loops run their passes the same way in `chain`
                    // and `tree` flow, so a `flow: tree` unnamed loop
                    // can have several passes in flight at once --
                    // one interval per logged (finished) pass at this
                    // path, plus one per pass still open right now,
                    // rather than a single interval for the whole
                    // loop (which undercounts a tree loop's own max
                    // at 1).
                    if model.has_events() && plan.is_unnamed_loop_body_path(&r.path) {
                        let is_agent = is_agent_row(r, flat, plan);
                        let offsets = model.step_interval_offsets(run_start_ts);
                        let mut out: Vec<(f64, f64, bool, StatusKind)> = model
                            .step_intervals()
                            .iter()
                            .zip(offsets.iter())
                            .filter(|&(i, &(s, e))| i.path == r.path && !s.is_nan() && !e.is_nan())
                            .map(|(i, &(s, e))| {
                                (
                                    s,
                                    e,
                                    is_agent,
                                    if i.ok {
                                        StatusKind::Done
                                    } else {
                                        StatusKind::Failed
                                    },
                                )
                            })
                            .collect();
                        drop(offsets);
                        let end_now = duration_seconds(run_start_ts, now);
                        for start_ts in model.open_starts_at(&r.path) {
                            if let Some(s) = duration_seconds(run_start_ts, start_ts) {
                                out.push((s, end_now.unwrap_or(s), is_agent, StatusKind::Running));
                            }
                        }
                        return out;
                    }
                    let Some(start) = row_run_relative_start(r, flat, model, run_start_ts) else {
                        return Vec::new();
                    };
                    let end = if r.status.kind == StatusKind::Running {
                        duration_seconds(run_start_ts, now).unwrap_or(start)
                    } else {
                        start + r.duration_s.unwrap_or(0.0)
                    };
                    vec![(start, end, is_agent_row(r, flat, plan), r.status.kind)]
                })
                .collect();
            let in_flight_intervals: Vec<(f64, f64)> =
                leaf_intervals.iter().map(|&(s, e, _, _)| (s, e)).collect();
            let running_starts: Vec<f64> = leaf_intervals
                .iter()
                .filter(|&&(_, _, _, status)| status == StatusKind::Running)
                .map(|&(s, _, _, _)| s)
                .collect();
            let events_points = crate::time::event_points(model, run_start_ts, &running_starts);
            let decisions = crate::time::decision_points(story_lines, run_start_ts);
            let running_leaf_is_agent: Vec<bool> = leaf_intervals
                .iter()
                .filter(|&&(_, _, _, status)| status == StatusKind::Running)
                .map(|&(_, _, is_agent, _)| is_agent)
                .collect();
            // The strip's own rate over the last minute, read from
            // the same points it draws from -- "quiet = agent" is
            // about the rate having actually dropped, not merely
            // about an agent happening to be the only thing running.
            // The running leaf's own start event is excluded: without
            // that, an agent that had just started would read
            // "quiet" as false from its own start alone.
            let recent_rate = now_s.map_or(0u64, |now_offset| {
                events_points
                    .iter()
                    .filter(|&&(t, _)| t >= now_offset - 60.0 && !running_starts.contains(&t))
                    .map(|&(_, w)| w)
                    .sum()
            });
            TimeStrips {
                events_points,
                events_from_state: !model.has_events(),
                decisions,
                in_flight_intervals,
                queued: crate::time::queued_now(flat),
                quiet_agent: crate::time::quiet_agent(&running_leaf_is_agent, recent_rate),
            }
        },
    );

    let selected = selected_path
        .and_then(|path| rows.iter().find(|r| r.path == path))
        .map(|row| build_focused_step(row, flat, plan, model, state, now));
    let zoom_extra = selected_path
        .and_then(|path| build_time_zoom_extra(path, &display_rows, plan, header, now_s));

    let vs_usual = build_vs_usual_pane(rows, flat, plan, model, header, now);

    TimeScreen {
        rows: display_rows,
        axis,
        now_s,
        planned,
        strips,
        selected,
        zoom_extra,
        vs_usual,
    }
}

fn row_run_relative_start(
    row: &Row,
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    run_start_ts: &str,
) -> Option<f64> {
    let start_ts = flat
        .get(&row.path)
        .and_then(|n| n.created_at.as_deref())
        .or_else(|| model.event_started_at(&row.path))?;
    duration_seconds(run_start_ts, start_ts)
}

fn build_vs_usual_pane(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    model: &RunModel,
    header: &crate::render::Header,
    now: &str,
) -> TimeVsUsualPane {
    if let Some(too_few) = crate::time::vs_usual_too_few_runs(header) {
        return TimeVsUsualPane {
            too_few_runs: Some(too_few),
            earlier_versions: crate::time::vs_usual_is_earlier_versions(header),
            ..Default::default()
        };
    }
    let earlier_versions = crate::time::vs_usual_is_earlier_versions(header);
    if header.run_start_ts.is_none() {
        return TimeVsUsualPane {
            earlier_versions,
            ..Default::default()
        };
    }
    // The same "a loop's own passes never appear as the base tree's
    // own rows" rule `draw_time_timeline` applies when drawing the
    // timeline itself -- history keeps one figure per loop step
    // across every pass (summed for a chain, overlap-merged for a
    // tree), so comparing one of its own passes against that figure
    // compares a part against the whole. A collapsed loop's pass
    // rows (breadcrumbed or not) stay out of vs usual entirely, same
    // as an expanded loop's `TimeDisplayRow::Pass` rows, which never
    // reach this `rows` list at all.
    let loop_prefixes: Vec<String> = rows
        .iter()
        .filter(|r| r.kind == RowKind::Loop)
        .map(|r| format!("{}.", r.path))
        .collect();
    let mut vs_rows = Vec::new();
    // "earlier versions" pools a different count per step (issue
    // #650) -- the largest pooled count among the rows actually shown
    // stands in for the footer's own `from N runs`, since `header.
    // history_same_hash_count` is the *current* hash's own count,
    // always below the threshold whenever `earlier_versions` is true
    // by definition, and would otherwise read "from 0 runs" for
    // figures that are not from 0 runs at all.
    let mut pooled_run_count: Option<usize> = None;
    for row in rows {
        if loop_prefixes
            .iter()
            .any(|p| row.path.starts_with(p.as_str()))
        {
            continue;
        }
        let Some(key) = crate::history::display_key(&row.path, plan) else {
            continue;
        };
        let Some(stats) = header.history_step_usual.get(&key) else {
            continue;
        };
        let Some(p50) = stats.busy_p50_s else {
            continue;
        };
        let running = row.status.kind == StatusKind::Running;
        let now_s = if running {
            let start_ts = flat
                .get(&row.path)
                .and_then(|n| n.created_at.as_deref())
                .or_else(|| model.event_started_at(&row.path));
            match start_ts.and_then(|s| duration_seconds(s, now)) {
                Some(s) => s,
                None => continue,
            }
        } else {
            match row.duration_s {
                Some(s) => s,
                None => continue,
            }
        };
        let Some(mark) = crate::time::vs_usual_mark(now_s, stats, running) else {
            continue;
        };
        pooled_run_count = Some(pooled_run_count.unwrap_or(0).max(stats.count));
        vs_rows.push(TimeVsUsualRow {
            path: row.path.clone(),
            label: row.label.clone(),
            now_s,
            running,
            p50_s: p50,
            mark,
        });
    }
    let finish_ts = if earlier_versions {
        None
    } else {
        header.history_eta_ts.clone()
    };
    TimeVsUsualPane {
        rows: vs_rows,
        earlier_versions,
        too_few_runs: None,
        finish_ts,
        usual_low_ts: header.history_eta_low_ts.clone(),
        usual_high_ts: header.history_eta_high_ts.clone(),
        from_runs: if earlier_versions {
            pooled_run_count
        } else {
            header.history_same_hash_count
        },
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
            tokens_sent: 1000,
            tokens_received: 500,
            innermost_loop_eta_s: Some(360.0),
            innermost_loop_eta_ts: Some("2026-10-08T11:58:00Z".to_string()),
            history_eta_ts: None,
            history_eta_past_usual: false,
            history_eta_remaining_s: None,
            history_eta_low_ts: None,
            history_eta_high_ts: None,
            history_vs_usual_s: None,
            history_usual_total_s: None,
            history_same_hash_count: None,
            history_step_usual: Default::default(),
            latest_report: None,
            engine_alive: true,
        }
    }

    #[test]
    fn screen_from_key_only_accepts_available_screens() {
        assert_eq!(Screen::from_key('0'), Some(Screen::Overview));
        assert_eq!(Screen::from_key('1'), Some(Screen::Time));
        assert_eq!(Screen::from_key('2'), Some(Screen::Flow));
        assert_eq!(Screen::from_key('3'), Some(Screen::Cost));
        assert_eq!(Screen::from_key('x'), None);
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
            Some(Screen::Overview),
        );
        let text = header_line_text(&segments);
        assert!(text.contains("feature-0417.yml"));
        assert!(text.contains("running"));
        assert!(text.contains("47m57s"));
        assert!(text.contains("11:52:55"));
        assert!(text.contains("loop ETA ~11:58"));
        assert!(text.contains("[0 overview]"));
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
            Some(Screen::Overview),
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
            Some(Screen::Overview),
        );
        let text = header_line_text(&segments);
        assert!(text.contains("ETA ?"), "{text}");
        assert!(!text.contains("loop ETA"), "{text}");
    }

    #[test]
    fn time_card_shows_the_history_eta_vs_usual_and_usual_range_once_there_is_enough_history() {
        let mut header = base_header();
        header.history_eta_ts = Some("2026-10-08T12:05:00Z".to_string());
        header.history_eta_remaining_s = Some(720.0);
        header.history_vs_usual_s = Some(180.0);
        header.history_usual_total_s = Some(2700.0);
        header.history_eta_low_ts = Some("2026-10-08T11:58:00Z".to_string());
        header.history_eta_high_ts = Some("2026-10-08T12:09:00Z".to_string());
        header.history_same_hash_count = Some(7);
        let card = build_time_card(&header, &[]);
        assert!(card.eta_from_history);
        assert!(!card.eta_past_usual);
        assert_eq!(card.eta_ts, header.history_eta_ts);
        assert_eq!(card.eta_remaining_s, Some(720.0));
        assert_eq!(card.vs_usual_s, Some(180.0));
        assert_eq!(card.usual_total_s, Some(2700.0));
        assert_eq!(card.usual_low_ts, header.history_eta_low_ts);
        assert_eq!(card.usual_high_ts, header.history_eta_high_ts);
        // Once there's a real ETA, the run count is enough on its
        // own (via `ETA ~`) -- `history: N of 3 runs` is only for
        // when there isn't one yet.
        assert_eq!(card.history_count, None);
    }

    #[test]
    fn time_card_shows_a_run_count_instead_of_an_eta_below_the_threshold() {
        let mut header = base_header();
        header.history_same_hash_count = Some(1);
        let card = build_time_card(&header, &[]);
        assert!(!card.eta_from_history);
        assert_eq!(card.eta_ts, header.innermost_loop_eta_ts);
        assert_eq!(card.history_count, Some(1));
        assert_eq!(card.vs_usual_s, None);
    }

    #[test]
    fn time_card_shows_a_zero_run_count_on_a_documents_very_first_ok_run() {
        let mut header = base_header();
        header.history_same_hash_count = Some(0);
        let card = build_time_card(&header, &[]);
        assert!(!card.eta_from_history);
        assert_eq!(
            card.history_count,
            Some(0),
            "history is on and collecting, even with no runs of this hash yet"
        );
    }

    #[test]
    fn time_card_shows_no_run_count_when_history_hasnt_resolved_a_document_yet() {
        let mut header = base_header();
        header.history_same_hash_count = None;
        let card = build_time_card(&header, &[]);
        assert!(!card.eta_from_history);
        assert_eq!(card.history_count, None);
    }

    #[test]
    fn time_card_shows_no_eta_at_all_once_past_the_usual_end() {
        let mut header = base_header();
        header.history_eta_past_usual = true;
        header.history_same_hash_count = Some(7);
        let card = build_time_card(&header, &[]);
        assert!(card.eta_from_history);
        assert!(card.eta_past_usual);
        assert_eq!(card.eta_ts, None);
        assert_eq!(
            card.history_count, None,
            "a run count is only shown below the ETA's own threshold, not past usual"
        );
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
            Some(Screen::Overview),
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
            Some(Screen::Overview),
        );
        let wide_text = header_line_text(&wide);
        let narrow = header_line(
            &header,
            "2026-10-08T11:52:55Z",
            &utc(),
            100,
            true,
            0,
            Some(Screen::Overview),
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
            Some(Screen::Overview),
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
            Some(Screen::Overview),
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
                "⠔⠒⠒⠤⠤⠔⠒⠒ feature-0417.yml · ♥ running 47m57s · 11:52:55 · loop ETA ~11:58   [0 overview]  1 time  2 flow  3 cost",
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
                Some(Screen::Overview),
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
                Some(Screen::Overview),
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
            Some(Screen::Overview),
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
    /// event, and the model from the compiled plan's own prompt op --
    /// neither of which a state-only read could ever answer.
    #[test]
    fn now_pane_for_an_event_only_step_shows_since_and_model_from_the_plan() {
        let plan = tree_loop_with_prompt_plan();
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Dispatch {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime.fan".to_string(),
            branches: 2,
            concurrency: Some(2),
        });
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.ask".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let flat = flat_from(state.clone());
        let statuses = model.observe(&state, &plan, ProcessState::Running);
        // The same "every path either side has heard of" view
        // `render::all_observed_paths` builds -- an event-only path
        // (confirmed running by `statuses` alone) never has a `flat`
        // entry of its own, so `flat.keys()` alone would miss it.
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
            "2026-01-01T00:06:12Z",
            &model,
            None,
        );
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            Some(&state),
            &base_header(),
            &[],
            None,
            "2026-01-01T00:06:12Z",
        );
        match pane.focus {
            NowFocus::Running(step, _) => {
                assert_eq!(step.since_ts.as_deref(), Some("2026-01-01T00:00:00Z"));
                assert!((step.elapsed_s.unwrap() - 372.0).abs() < 1e-9);
                assert_eq!(step.provider_or_model.as_deref(), Some("scripted-model"));
            }
            other => panic!("expected Running, got {other:?}"),
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
        let folded = crate::fold::fold_rows(rows.clone(), true, &Default::default());
        let screen = build_flow_screen(
            &folded,
            &rows,
            &flat,
            &plan,
            &model,
            Some(&state),
            &[],
            Some("prime.check"),
            &utc(),
            "2026-10-08T11:00:10Z",
        );
        assert!(screen.selected.is_none());
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

    #[test]
    fn several_running_steps_report_the_extra_count() {
        let rows = vec![
            Row {
                path: "prime.a".to_string(),
                depth: 1,
                label: "a".to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(1.0),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: false,
            },
            Row {
                path: "prime.b".to_string(),
                depth: 1,
                label: "b".to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(1.0),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: false,
            },
        ];
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let header = base_header();
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            None,
            &header,
            &[],
            None,
            "2026-10-08T11:00:00Z",
        );
        match pane.focus {
            NowFocus::Running(_, more) => assert_eq!(more, 1),
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn nested_running_containers_count_as_one_running_step() {
        fn running_row(path: &str, depth: usize) -> Row {
            Row {
                path: path.to_string(),
                depth,
                label: path.rsplit('.').next().unwrap_or(path).to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(1.0),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: true,
            }
        }
        // `review > fix > agent`: a container is "running" the moment
        // any descendant is (`completed_at` unset until every child
        // has one), so all three rows read `Running` here even though
        // only `agent` (the leaf) is a real step actually in flight.
        let rows = vec![
            running_row("prime.review", 1),
            running_row("prime.review.fix", 2),
            running_row("prime.review.fix.agent", 3),
        ];
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let header = base_header();
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            None,
            &header,
            &[],
            None,
            "2026-10-08T11:00:00Z",
        );
        match pane.focus {
            NowFocus::Running(step, more) => {
                assert_eq!(step.path, "prime.review.fix.agent");
                assert_eq!(more, 0, "only the leaf is a real running step");
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn a_running_steps_own_usual_busy_time_is_filled_in_from_history() {
        fn running_row(path: &str) -> Row {
            Row {
                path: path.to_string(),
                depth: 1,
                label: path.to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(1.0),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: false,
            }
        }
        let rows = vec![running_row("prime.slow")];
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let mut header = base_header();
        header.history_step_usual.insert(
            "slow".to_string(),
            crate::history::StepStats {
                count: 5,
                busy_p50_s: Some(300.0),
                busy_p10_s: None,
                busy_p90_s: Some(540.0),
                ..Default::default()
            },
        );
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            None,
            &header,
            &[],
            None,
            "2026-10-08T11:00:00Z",
        );
        match pane.focus {
            NowFocus::Running(step, _) => {
                assert_eq!(step.usual_busy_p50_s, Some(300.0));
                assert_eq!(step.usual_busy_p90_s, Some(540.0));
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn the_running_focus_prefers_the_deepest_row_over_the_root() {
        fn running_row(path: &str, depth: usize) -> Row {
            Row {
                path: path.to_string(),
                depth,
                label: path.to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(1.0),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: false,
            }
        }
        let rows = vec![running_row("prime", 0), running_row("prime.a.b", 2)];
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let header = base_header();
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            None,
            &header,
            &[],
            None,
            "2026-10-08T11:00:00Z",
        );
        match pane.focus {
            NowFocus::Running(step, _) => assert_eq!(step.path, "prime.a.b"),
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn nested_failed_containers_count_as_one_failure() {
        fn failed_row(path: &str, depth: usize) -> Row {
            Row {
                path: path.to_string(),
                depth,
                label: path.rsplit('.').next().unwrap_or(path).to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::FailedHandled,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(1.0),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: true,
            }
        }
        let rows = vec![
            failed_row("prime.review", 1),
            failed_row("prime.review.fix", 2),
            failed_row("prime.review.fix.agent", 3),
        ];
        let flat = BTreeMap::new();
        let model = RunModel::new();
        let header = base_header();
        let card = build_flow_card(&rows, &rows, &flat, &model, &header);
        assert_eq!(card.failures, 1, "only the leaf is a real failure");
    }

    #[test]
    fn decisions_come_from_a_named_ifs_branch() {
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.check".to_string(),
            NodeMeta {
                created_at: Some("2026-10-08T11:00:00Z".to_string()),
                completed_at: Some("2026-10-08T11:00:01Z".to_string()),
                error: None,
                branch: Some("then".to_string()),
                progress_done: None,
                progress_total: None,
                progress_eta_s: None,
                live: None,
                value: Value::Null,
                meta: json!({"branch": "then"}),
            },
        );
        let rows = Vec::new();
        let model = RunModel::new();
        let header = base_header();
        let card = build_flow_card(&rows, &rows, &flat, &model, &header);
        assert_eq!(card.recent.len(), 1);
        match &card.recent[0] {
            FlowEvent::Decision { name, branch, .. } => {
                assert_eq!(name, "check");
                assert_eq!(branch, "then");
            }
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    #[test]
    fn quick_steps_are_counted_from_the_unfolded_rows() {
        fn quick_row(path: &str) -> Row {
            Row {
                path: path.to_string(),
                depth: 1,
                label: path.to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Done,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: Some(0.3),
                loop_progress: None,
                dim: Some("shell".to_string()),
                has_children: false,
            }
        }
        let unfolded = vec![
            quick_row("prime.a"),
            quick_row("prime.b"),
            quick_row("prime.c"),
        ];
        let flat = BTreeMap::new();
        let model = RunModel::new();
        let header = base_header();
        let card = build_flow_card(&unfolded, &unfolded, &flat, &model, &header);
        let (count, total_s) = card.quick_steps.expect("three quick steps");
        assert_eq!(count, 3);
        assert!((total_s - 0.9).abs() < 1e-9);
    }

    #[test]
    fn cost_is_hidden_until_any_effect_reports_it() {
        let header = base_header();
        let token_bar = TokenBar::default();
        let flat = BTreeMap::new();
        let no_cost = build_cost_card(
            &[],
            &header,
            Some(&json!({"runtime": {"last_run": {"totals": {}}}})),
            &flat,
            &PlanTree::empty(),
            &token_bar,
            "2026-10-08T11:52:55Z",
        );
        assert_eq!(no_cost.cost_usd, None);
        let with_cost = build_cost_card(
            &[],
            &header,
            Some(&json!({"runtime": {"last_run": {"totals": {"cost_usd": 14.2}}}})),
            &flat,
            &PlanTree::empty(),
            &token_bar,
            "2026-10-08T11:52:55Z",
        );
        assert_eq!(with_cost.cost_usd, Some(14.2));
    }

    #[test]
    fn cost_sums_from_meta_cost_usd_mid_run_before_totals_exist() {
        let header = base_header();
        let token_bar = TokenBar::default();
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.a".to_string(),
            NodeMeta {
                created_at: Some("2026-10-08T11:04:58Z".to_string()),
                completed_at: Some("2026-10-08T11:05:58Z".to_string()),
                error: None,
                branch: None,
                progress_done: None,
                progress_total: None,
                progress_eta_s: None,
                live: None,
                value: Value::Null,
                meta: json!({"cost_usd": 1.5}),
            },
        );
        flat.insert(
            "prime.b".to_string(),
            NodeMeta {
                created_at: Some("2026-10-08T11:06:00Z".to_string()),
                completed_at: Some("2026-10-08T11:07:00Z".to_string()),
                error: None,
                branch: None,
                progress_done: None,
                progress_total: None,
                progress_eta_s: None,
                live: None,
                value: Value::Null,
                meta: json!({"cost_usd": 0.75}),
            },
        );
        let card = build_cost_card(
            &[],
            &header,
            None,
            &flat,
            &PlanTree::empty(),
            &token_bar,
            "2026-10-08T11:52:55Z",
        );
        assert_eq!(card.cost_usd, Some(2.25));
    }

    #[test]
    fn tokens_over_time_buckets_by_each_effects_own_completion() {
        let mut header = base_header();
        header.run_start_ts = Some("2026-10-08T11:00:00Z".to_string());
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.a".to_string(),
            NodeMeta {
                created_at: Some("2026-10-08T11:00:00Z".to_string()),
                completed_at: Some("2026-10-08T11:00:10Z".to_string()),
                error: None,
                branch: None,
                progress_done: None,
                progress_total: None,
                progress_eta_s: None,
                live: None,
                value: Value::Null,
                meta: json!({"tokens_sent": 100, "tokens_received": 0}),
            },
        );
        flat.insert(
            "prime.b".to_string(),
            NodeMeta {
                created_at: Some("2026-10-08T11:00:50Z".to_string()),
                completed_at: Some("2026-10-08T11:00:55Z".to_string()),
                error: None,
                branch: None,
                progress_done: None,
                progress_total: None,
                progress_eta_s: None,
                live: None,
                value: Value::Null,
                meta: json!({"tokens_sent": 0, "tokens_received": 40}),
            },
        );
        let buckets = build_tokens_over_time(&header, &flat, "2026-10-08T11:01:00Z");
        assert_eq!(buckets.len(), 12);
        assert_eq!(buckets.iter().sum::<u64>(), 140);
        // A 60s span in 12 buckets is 5s each: the first completion
        // (at 10s) lands in bucket 2, well before the second (at 55s,
        // the last bucket).
        assert!(
            buckets[2] > 0,
            "the early completion lands in an early bucket: {buckets:?}"
        );
        assert!(
            buckets[buckets.len() - 1] > 0,
            "the late completion lands in a late bucket: {buckets:?}"
        );
    }

    #[test]
    fn a_finished_run_shows_the_ended_summary() {
        let mut header = base_header();
        header.status = RunStatus::Ok;
        header.run_end_ts = Some("2026-10-08T11:58:00Z".to_string());
        let rows = Vec::new();
        let flat = BTreeMap::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            None,
            &header,
            &[],
            None,
            "2026-10-08T11:58:00Z",
        );
        match pane.focus {
            NowFocus::Ended(summary) => assert_eq!(summary.outcome, RunStatus::Ok),
            other => panic!("expected Ended, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_run_shows_the_ended_summary_too() {
        let mut header = base_header();
        header.status = RunStatus::Failed;
        header.run_end_ts = Some("2026-10-08T11:58:00Z".to_string());
        let rows = Vec::new();
        let flat = BTreeMap::new();
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let pane = build_now_pane(
            &rows,
            &flat,
            &plan,
            &model,
            None,
            &header,
            &[],
            None,
            "2026-10-08T11:58:00Z",
        );
        match pane.focus {
            NowFocus::Ended(summary) => assert_eq!(summary.outcome, RunStatus::Failed),
            other => panic!("expected Ended, got {other:?}"),
        }
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
        let step = build_focused_step(&row, &flat, &plan, &model, None, "2026-10-08T11:52:55Z");
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
        let step = build_focused_step(&row, &flat, &plan, &model, None, "2026-10-08T11:52:55Z");
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
        let step = build_focused_step(&row, &flat, &plan, &model, None, "2026-10-08T11:52:56Z");
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
        let step = build_focused_step(&row, &flat, &plan, &model, None, "2026-10-08T11:52:55Z");
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

    #[test]
    fn build_time_screen_shows_a_running_rows_bar_and_the_now_line() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"}},
            "b": {"value": null, "meta": {"created_at": "2026-01-01T00:00:10Z", "completed_at": null}}
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            30.0,
            "2026-01-01T00:00:30Z",
            &[],
            true,
            &Default::default(),
        );
        let screen = build_time_screen(
            &rendered.rows,
            &flat_from(state.clone()),
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:30Z",
        );
        assert!(screen.axis.show_now);
        assert_eq!(screen.now_s, Some(30.0));
        let running = screen
            .rows
            .iter()
            .find_map(|r| match r {
                TimeDisplayRow::Row(row) if row.path == "prime.b" => Some(row),
                _ => None,
            })
            .expect("row b is in the timeline");
        assert_eq!(running.start_s, Some(10.0));
        assert_eq!(running.status.kind, StatusKind::Running);
    }

    /// A finished step's zoom-pane duration is its own `completed_at`
    /// minus `created_at`, never `now - created_at` -- `now` here is
    /// long after the whole run ended, which a stale computation
    /// would otherwise fold into the step's own duration.
    #[test]
    fn build_time_screen_a_finished_steps_duration_ignores_how_long_ago_now_is() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:20Z"
            },
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"}},
            "b": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:10Z", "completed_at": "2026-01-01T00:00:20Z"}}
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            20.0,
            "2026-01-01T00:00:20Z",
            &[],
            true,
            &Default::default(),
        );
        let screen = build_time_screen(
            &rendered.rows,
            &flat_from(state.clone()),
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some("prime.a"),
            &Default::default(),
            "2026-02-15T12:00:00Z",
        );
        let selected = screen.selected.expect("prime.a is selected");
        assert_eq!(
            selected.elapsed_s,
            Some(10.0),
            "a finished step's own duration, not now - its start"
        );
    }

    fn each_loop_plan() -> PlanTree {
        use electricity_bytecode::{
            EffectPath, LeafKind, NodeKind, OnError, Op, Program, Region, ToolOp,
        };
        let root_path = EffectPath::root();
        let loop_path = root_path.push_name("items");
        let pass0 = loop_path.push_pass(electricity_bytecode::LoopId(0));
        let tool = Op {
            path: pass0.push_name("nap"),
            name: Some("nap".to_string()),
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
                prompt_file: None,
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
                    path: loop_path.clone(),
                    name: Some("items".to_string()),
                    kind: NodeKind::Control(Region::Loop {
                        name: Some("items".to_string()),
                        spec: electricity_bytecode::LoopSpec::Each {
                            in_path: "prime.xs".to_string(),
                            as_name: "x".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![tool],
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

    /// History keeps one figure per loop step across every pass
    /// (issue #650's design), so a pass's own child row must never be
    /// compared against it in vs usual -- only the full timeline's
    /// own rows are, and a loop's passes never appear there at all.
    #[test]
    fn build_vs_usual_pane_never_compares_a_loop_passs_own_child() {
        let plan = each_loop_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"
            },
            "items": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"
                },
                "iter_0": {
                    "nap": {"value": "ok", "meta": {
                        "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:05Z",
                        "provider": "shell"
                    }}
                },
                "iter_1": {
                    "nap": {"value": "ok", "meta": {
                        "created_at": "2026-01-01T00:00:05Z", "completed_at": "2026-01-01T00:00:10Z",
                        "provider": "shell"
                    }}
                }
            }
        }});
        let mut rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            10.0,
            "2026-01-01T00:00:10Z",
            &[],
            true,
            &Default::default(),
        );
        rendered.header.history_same_hash_count = Some(3);
        rendered
            .header
            .history_step_usual
            .insert("items.iter_*.nap".to_string(), stats_usual(100.0));
        let flat = flat_from(state.clone());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:10Z",
        );
        assert!(
            screen.vs_usual.rows.is_empty(),
            "a pass's own child must never appear in vs usual: {:?}",
            screen.vs_usual.rows
        );
    }

    fn stats_usual(busy_p50: f64) -> crate::history::StepStats {
        crate::history::StepStats {
            count: 3,
            busy_p50_s: Some(busy_p50),
            ..Default::default()
        }
    }

    /// "earlier versions" means below the current hash's own
    /// run-count threshold, so `header.history_same_hash_count`
    /// alone would always read `from 0 runs` here -- the footer must
    /// instead read however many runs the pooled figures it is
    /// actually showing came from.
    #[test]
    fn build_vs_usual_pane_reads_the_pooled_count_under_earlier_versions_not_zero() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"
            },
            "nap": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z",
                "provider": "shell"
            }}
        }});
        let mut rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            10.0,
            "2026-01-01T00:00:10Z",
            &[],
            true,
            &Default::default(),
        );
        rendered.header.history_same_hash_count = Some(0);
        rendered.header.history_step_usual.insert(
            "nap".to_string(),
            crate::history::StepStats {
                count: 7,
                busy_p50_s: Some(10.0),
                ..Default::default()
            },
        );
        let flat = flat_from(state.clone());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:10Z",
        );
        assert!(screen.vs_usual.earlier_versions);
        assert!(
            !screen.vs_usual.rows.is_empty(),
            "the pooled row must show: {:?}",
            screen.vs_usual.rows
        );
        assert_eq!(
            screen.vs_usual.from_runs,
            Some(7),
            "must read the pooled count the figures actually came from, never the current hash's 0 runs"
        );
    }

    #[test]
    fn build_time_screen_expands_a_loop_row_when_asked() {
        let plan = each_loop_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "items": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
                "iter_0": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:05Z"}},
                "iter_1": {"value": null, "meta": {"created_at": "2026-01-01T00:00:05Z", "completed_at": null}}
            }
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            10.0,
            "2026-01-01T00:00:10Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = flat_from(state.clone());
        let mut expanded = std::collections::BTreeSet::new();
        expanded.insert("prime.items".to_string());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &expanded,
            "2026-01-01T00:00:10Z",
        );
        let passes: Vec<&TimeLoopPassRow> = screen
            .rows
            .iter()
            .filter_map(|r| match r {
                TimeDisplayRow::Pass(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].label, "pass 1");
        assert_eq!(passes[1].label, "pass 2");
    }

    /// An unnamed loop's body leaf shares its row with the loop
    /// itself (`RowKind::Tool`, never `RowKind::Loop`), so
    /// `build_time_screen` -- not only `expand_loop` directly -- must
    /// still add its `TimeDisplayRow::Pass` rows once `--events` gave
    /// it more than one interval. A sibling at the same parent path
    /// that is not part of the loop's body must never expand, even if
    /// asked to.
    fn unnamed_loop_plan_with_sibling() -> PlanTree {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopFlow, LoopSpec, NodeKind, OnError, Op, ParamNode, Program,
            Region, ToolOp,
        };
        fn tool(path: EffectPath, name: &str) -> Op {
            Op {
                path,
                name: Some(name.to_string()),
                kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                    provider: "shell".to_string(),
                    params: ParamNode::Literal(electricity_value::Value::None),
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
            }
        }
        let root_path = EffectPath::root();
        let root = Op {
            path: root_path.clone(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![
                    Op {
                        path: root_path.clone(),
                        name: None,
                        kind: NodeKind::Control(Region::Loop {
                            name: None,
                            spec: LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![tool(root_path.clone().push_name("u_nap"), "u_nap")],
                                overlay: true,
                            }),
                            flow: LoopFlow::Chain,
                            max_concurrency: None,
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    },
                    tool(root_path.push_name("a"), "a"),
                ],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        PlanTree::from_program(&Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        })
    }

    #[test]
    fn build_time_screen_expands_an_unnamed_loops_body_leaf_but_never_its_sibling() {
        let plan = unnamed_loop_plan_with_sibling();
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(5_000),
            error: None,
        });
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:12Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(7_000),
            error: None,
        });
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "a": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:12Z", "completed_at": "2026-01-01T00:00:13Z"
            }}
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            13.0,
            "2026-01-01T00:00:13Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = flat_from(state.clone());
        let mut expanded = std::collections::BTreeSet::new();
        expanded.insert("prime.u_nap".to_string());
        expanded.insert("prime.a".to_string());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &expanded,
            "2026-01-01T00:00:13Z",
        );
        let pass_labels_after = |path: &str, rows: &[TimeDisplayRow]| -> Vec<String> {
            let mut out = Vec::new();
            let mut seen_row = false;
            for row in rows {
                match row {
                    TimeDisplayRow::Row(r) if r.path == path => seen_row = true,
                    TimeDisplayRow::Row(_) => seen_row = false,
                    TimeDisplayRow::Pass(p) if seen_row => out.push(p.label.clone()),
                    TimeDisplayRow::Pass(_) => {}
                }
            }
            out
        };
        assert_eq!(
            pass_labels_after("prime.u_nap", &screen.rows),
            vec!["#1".to_string(), "#2".to_string()],
            "the loop's own body leaf must expand into its own pass rows: {:?}",
            screen.rows
        );
        assert!(
            pass_labels_after("prime.a", &screen.rows).is_empty(),
            "a sibling outside the loop's body must never expand, even if asked to: {:?}",
            screen.rows
        );
    }

    /// Collapsed, an unnamed loop's own row spans its first pass's
    /// own start through its last pass's own (state-sourced) end,
    /// carries the real finished `⟲ 2/2` once the loop itself has
    /// ended, and the whole span counts once in `in flight`.
    #[test]
    fn build_time_screen_collapsed_unnamed_loop_row_spans_first_pass_to_last() {
        let plan = unnamed_loop_plan_with_sibling();
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(1),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(5_000),
            error: None,
        });
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:12Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(7_000),
            error: None,
        });
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:13Z"
            },
            "u_nap": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:05Z", "completed_at": "2026-01-01T00:00:12Z",
                "provider": "shell"
            }},
            "a": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:12Z", "completed_at": "2026-01-01T00:00:13Z"
            }}
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            13.0,
            "2026-01-01T00:00:13Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = flat_from(state.clone());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:13Z",
        );
        let row = screen
            .rows
            .iter()
            .find_map(|r| match r {
                TimeDisplayRow::Row(tr) if tr.path == "prime.u_nap" => Some(tr),
                _ => None,
            })
            .expect("the collapsed row must still be there");
        assert_eq!(
            row.start_s,
            Some(0.0),
            "the row starts at the first pass's own start, not the last pass's: {row:?}"
        );
        assert_eq!(
            row.duration_s,
            Some(12.0),
            "the row's own span runs through the last pass's own (state-sourced) end: {row:?}"
        );
        let progress = row
            .loop_progress
            .expect("an unnamed loop with logged passes has progress");
        assert_eq!(progress.done, 2);
        assert_eq!(
            progress.total,
            Some(2),
            "the loop itself has finished, so the real total is known"
        );
        // One interval per logged pass (two, for u_nap), plus one
        // for the unrelated sibling `a` -- sequential, so none
        // overlap, and the true max stays 1.
        assert_eq!(
            screen.strips.in_flight_intervals.len(),
            3,
            "{:?}",
            screen.strips.in_flight_intervals
        );
        let heights =
            crate::time::in_flight_heights(0.0, 13.0, 13, &screen.strips.in_flight_intervals);
        assert_eq!(
            heights.iter().copied().max(),
            Some(1),
            "sequential passes never overlap: {heights:?}"
        );
    }

    fn unnamed_loop_plan_under_tree_parallel() -> PlanTree {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopFlow, LoopSpec, NodeKind, OnError, Op, ParamNode, Program,
            Region, ToolOp,
        };
        fn tool(path: EffectPath, name: &str) -> Op {
            Op {
                path,
                name: Some(name.to_string()),
                kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                    provider: "shell".to_string(),
                    params: ParamNode::Literal(electricity_value::Value::None),
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
            }
        }
        let root_path = EffectPath::root();
        let fan_path = root_path.clone().push_name("fan");
        let a_path = fan_path.clone().push_name("a");
        let u_nap_path = fan_path.clone().push_name("u_nap");
        let root = Op {
            path: root_path.clone(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: fan_path.clone(),
                    name: Some("fan".to_string()),
                    kind: NodeKind::Control(Region::Parallel {
                        branches: vec![
                            tool(a_path, "a"),
                            Op {
                                path: fan_path.clone(),
                                name: None,
                                kind: NodeKind::Control(Region::Loop {
                                    name: None,
                                    spec: LoopSpec::Each {
                                        in_path: "prime.items".to_string(),
                                        as_name: "item".to_string(),
                                        truncate: false,
                                    },
                                    body: Box::new(Region::Block {
                                        ops: vec![tool(u_nap_path, "u_nap")],
                                        overlay: true,
                                    }),
                                    flow: LoopFlow::Chain,
                                    max_concurrency: None,
                                    max_iterations: None,
                                    min_iterations: 0,
                                    collect: None,
                                }),
                                on_error: OnError::Fail,
                                labels: None,
                                enabled: true,
                            },
                        ],
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
        };
        PlanTree::from_program(&Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        })
    }

    /// An unnamed loop reached directly as a tree `dynamic`'s own
    /// `Parallel` branch, never through `walk_block`'s transparent-op
    /// handling, must still be recognised as a loop body -- `record_
    /// op_entry` is the one place both contexts funnel their loop
    /// `PlanEntryKind` through.
    #[test]
    fn build_time_screen_expands_an_unnamed_loop_directly_under_a_tree_parallel() {
        let plan = unnamed_loop_plan_under_tree_parallel();
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.fan.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(1),
            path: "prime.fan.u_nap".to_string(),
            ok: true,
            ms: Some(5_000),
            error: None,
        });
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(2),
            path: "prime.fan.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:12Z".to_string(),
            id: Some(2),
            path: "prime.fan.u_nap".to_string(),
            ok: true,
            ms: Some(7_000),
            error: None,
        });
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "fan": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "completed_at": null, "flow": "tree"
                },
                "a": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z"
                }},
                "u_nap": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:05Z", "completed_at": "2026-01-01T00:00:12Z"
                }}
            }
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            13.0,
            "2026-01-01T00:00:13Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = flat_from(state.clone());
        let mut expanded = std::collections::BTreeSet::new();
        expanded.insert("prime.fan.u_nap".to_string());
        expanded.insert("prime.fan.a".to_string());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &expanded,
            "2026-01-01T00:00:13Z",
        );
        let pass_labels_after = |path: &str, rows: &[TimeDisplayRow]| -> Vec<String> {
            let mut out = Vec::new();
            let mut seen_row = false;
            for row in rows {
                match row {
                    TimeDisplayRow::Row(r) if r.path == path => seen_row = true,
                    TimeDisplayRow::Row(_) => seen_row = false,
                    TimeDisplayRow::Pass(p) if seen_row => out.push(p.label.clone()),
                    TimeDisplayRow::Pass(_) => {}
                }
            }
            out
        };
        assert_eq!(
            pass_labels_after("prime.fan.u_nap", &screen.rows),
            vec!["#1".to_string(), "#2".to_string()],
            "the loop's own body leaf must expand into its own pass rows: {:?}",
            screen.rows
        );
        assert!(
            pass_labels_after("prime.fan.a", &screen.rows).is_empty(),
            "a sibling outside the loop's body must never expand, even if asked to: {:?}",
            screen.rows
        );
    }

    fn unnamed_loop_inside_named_loops_pass_plan() -> PlanTree {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode,
            Program, Region, ToolOp,
        };
        fn tool(path: EffectPath, name: &str) -> Op {
            Op {
                path,
                name: Some(name.to_string()),
                kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                    provider: "shell".to_string(),
                    params: ParamNode::Literal(electricity_value::Value::None),
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
            }
        }
        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("items");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let u_nap_path = pass_path.clone().push_name("u_nap");
        let root = Op {
            path: root_path.clone(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: loop_path.clone(),
                    name: Some("items".to_string()),
                    kind: NodeKind::Control(Region::Loop {
                        name: Some("items".to_string()),
                        spec: LoopSpec::Each {
                            in_path: "prime.list".to_string(),
                            as_name: "item".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![Op {
                                path: pass_path.clone(),
                                name: None,
                                kind: NodeKind::Control(Region::Loop {
                                    name: None,
                                    spec: LoopSpec::Each {
                                        in_path: "prime.sub".to_string(),
                                        as_name: "s".to_string(),
                                        truncate: false,
                                    },
                                    body: Box::new(Region::Block {
                                        ops: vec![tool(u_nap_path.clone(), "u_nap")],
                                        overlay: true,
                                    }),
                                    flow: LoopFlow::Chain,
                                    max_concurrency: None,
                                    max_iterations: None,
                                    min_iterations: 0,
                                    collect: None,
                                }),
                                on_error: OnError::Fail,
                                labels: None,
                                enabled: true,
                            }],
                            overlay: true,
                        }),
                        flow: LoopFlow::Chain,
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
        };
        PlanTree::from_program(&Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        })
    }

    /// An unnamed loop inside a *named* loop's own pass carries the
    /// real pass's concrete `iter_N` in its row path, never the
    /// `iter_*` placeholder `PlanTree::is_unnamed_loop_body_path`'s
    /// own set is keyed by -- the lookup must normalise first.
    #[test]
    fn build_time_screen_expands_an_unnamed_loop_inside_a_named_loops_pass() {
        let plan = unnamed_loop_inside_named_loops_pass_plan();
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.items.iter_0.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(1),
            path: "prime.items.iter_0.u_nap".to_string(),
            ok: true,
            ms: Some(5_000),
            error: None,
        });
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:05Z".to_string(),
            id: Some(2),
            path: "prime.items.iter_0.u_nap".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:12Z".to_string(),
            id: Some(2),
            path: "prime.items.iter_0.u_nap".to_string(),
            ok: true,
            ms: Some(7_000),
            error: None,
        });
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "items": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "completed_at": null, "mode": "each"
                },
                "iter_0": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:00:00Z", "completed_at": null
                    },
                    "u_nap": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:00:05Z", "completed_at": "2026-01-01T00:00:12Z"
                    }}
                }
            }
        }});
        let rendered = crate::render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            13.0,
            "2026-01-01T00:00:13Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = flat_from(state.clone());
        let mut expanded = std::collections::BTreeSet::new();
        expanded.insert("prime.items.iter_0.u_nap".to_string());
        let screen = build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &expanded,
            "2026-01-01T00:00:13Z",
        );
        let pass_labels_after = |path: &str, rows: &[TimeDisplayRow]| -> Vec<String> {
            let mut out = Vec::new();
            let mut seen_row = false;
            for row in rows {
                match row {
                    TimeDisplayRow::Row(r) if r.path == path => seen_row = true,
                    TimeDisplayRow::Row(_) => seen_row = false,
                    TimeDisplayRow::Pass(p) if seen_row => out.push(p.label.clone()),
                    TimeDisplayRow::Pass(_) => {}
                }
            }
            out
        };
        assert_eq!(
            pass_labels_after("prime.items.iter_0.u_nap", &screen.rows),
            vec!["#1".to_string(), "#2".to_string()],
            "the inner unnamed loop's own body leaf must expand into its own pass rows: {:?}",
            screen.rows
        );
    }

    /// (`RowKind::is_leaf`), but it can have child rows of its own --
    /// without excluding those, "in flight" would count the `use`
    /// alongside the one child actually running inside it.
    #[test]
    fn build_time_screen_in_flight_excludes_a_use_with_a_running_child() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "review": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "completed_at": null
                },
                "nap": {"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:01Z", "completed_at": null
                }}
            }
        }});
        let flat = flat_from(state.clone());
        let rows = vec![
            Row {
                path: "prime".to_string(),
                depth: 0,
                label: "prime".to_string(),
                kind: RowKind::Root,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: true,
            },
            Row {
                path: "prime.review".to_string(),
                depth: 1,
                label: "review".to_string(),
                kind: RowKind::Use,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: true,
            },
            Row {
                path: "prime.review.nap".to_string(),
                depth: 2,
                label: "nap".to_string(),
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
            },
        ];
        let header = crate::render::Header {
            run_start_ts: Some("2026-01-01T00:00:00Z".to_string()),
            ..base_header()
        };
        let screen = build_time_screen(
            &rows,
            &flat,
            &plan,
            &model,
            &header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:02Z",
        );
        assert_eq!(
            screen.strips.in_flight_intervals.len(),
            1,
            "only the running child counts, not the use step around it: {:?}",
            screen.strips.in_flight_intervals
        );
    }

    /// An unnamed loop runs its passes the same way whether it's
    /// named or not (`flow: tree` passes can run concurrently) -- a
    /// single merged interval for the whole loop undercounted "in
    /// flight" at 1 for three passes genuinely running at once.
    #[test]
    fn build_time_screen_in_flight_counts_an_unnamed_tree_loops_concurrent_passes() {
        let plan = unnamed_loop_plan_with_sibling();
        let mut model = RunModel::new();
        for id in 1..=3 {
            model.observe_event(&crate::observe::Event::Start {
                ts: "2026-01-01T00:00:00Z".to_string(),
                id: Some(id),
                path: "prime.u_nap".to_string(),
            });
        }
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "u_nap": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            }}
        }});
        let flat = flat_from(state.clone());
        let rows = vec![
            Row {
                path: "prime".to_string(),
                depth: 0,
                label: "prime".to_string(),
                kind: RowKind::Root,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: true,
            },
            Row {
                path: "prime.u_nap".to_string(),
                depth: 1,
                label: "u_nap".to_string(),
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
            },
        ];
        let header = crate::render::Header {
            run_start_ts: Some("2026-01-01T00:00:00Z".to_string()),
            ..base_header()
        };
        let screen = build_time_screen(
            &rows,
            &flat,
            &plan,
            &model,
            &header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:05Z",
        );
        assert_eq!(
            screen.strips.in_flight_intervals.len(),
            3,
            "every concurrently open pass must count: {:?}",
            screen.strips.in_flight_intervals
        );
        let heights =
            crate::time::in_flight_heights(0.0, 5.0, 1, &screen.strips.in_flight_intervals);
        assert_eq!(heights, vec![3], "max overlap must read 3, not 1");
    }

    /// An agent is a `RowKind::Tool` with provider `agent`, never a
    /// `RowKind::Prompt` -- `quiet_agent` must read true for a lone
    /// running agent tool once its own recent rate has dropped, not
    /// only for an ordinary prompt.
    #[test]
    fn build_time_screen_quiet_agent_is_true_for_a_lone_running_agent_tool() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "agent_step": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                "provider": "agent"
            }}
        }});
        let flat = flat_from(state.clone());
        let rows = vec![
            Row {
                path: "prime".to_string(),
                depth: 0,
                label: "prime".to_string(),
                kind: RowKind::Root,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: true,
            },
            Row {
                path: "prime.agent_step".to_string(),
                depth: 1,
                label: "agent_step".to_string(),
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
            },
        ];
        let header = crate::render::Header {
            run_start_ts: Some("2026-01-01T00:00:00Z".to_string()),
            ..base_header()
        };
        // Past the agent's own start by more than the 60s window, so
        // only its own start event (excluded) would otherwise have
        // kept the rate above zero.
        let screen = build_time_screen(
            &rows,
            &flat,
            &plan,
            &model,
            &header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:02:00Z",
        );
        assert!(screen.strips.quiet_agent);
    }

    /// The agent's own start event must not itself count toward
    /// "recent" -- an agent that only just started would otherwise
    /// never read as quiet.
    #[test]
    fn build_time_screen_quiet_agent_excludes_the_agents_own_start_event() {
        let plan = PlanTree::empty();
        let model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null
            },
            "agent_step": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                "provider": "agent"
            }}
        }});
        let flat = flat_from(state.clone());
        let rows = vec![
            Row {
                path: "prime".to_string(),
                depth: 0,
                label: "prime".to_string(),
                kind: RowKind::Root,
                status: RowStatus {
                    kind: StatusKind::Running,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: true,
            },
            Row {
                path: "prime.agent_step".to_string(),
                depth: 1,
                label: "agent_step".to_string(),
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
            },
        ];
        let header = crate::render::Header {
            run_start_ts: Some("2026-01-01T00:00:00Z".to_string()),
            ..base_header()
        };
        // 30s after the agent's own start -- well inside the 60s
        // window, so this only reads quiet if that one event is
        // excluded.
        let screen = build_time_screen(
            &rows,
            &flat,
            &plan,
            &model,
            &header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            "2026-01-01T00:00:30Z",
        );
        assert!(screen.strips.quiet_agent);
    }

    fn time_row(
        path: &str,
        depth: usize,
        kind: RowKind,
        status_kind: StatusKind,
        start_s: Option<f64>,
        duration_s: Option<f64>,
        has_children: bool,
    ) -> crate::time::TimeRow {
        crate::time::TimeRow {
            path: path.to_string(),
            depth,
            label: path.rsplit('.').next().unwrap_or(path).to_string(),
            kind,
            status: RowStatus {
                kind: status_kind,
                skip_reason: None,
                retrying: false,
            },
            start_s,
            duration_s,
            loop_progress: None,
            has_children,
            loop_expandable: false,
            ghost: None,
        }
    }

    #[test]
    fn zoom_extra_lists_a_containers_direct_children_only() {
        let display_rows = vec![
            TimeDisplayRow::Row(time_row(
                "prime.review",
                1,
                RowKind::If,
                StatusKind::Running,
                Some(0.0),
                None,
                true,
            )),
            TimeDisplayRow::Row(time_row(
                "prime.review.agent",
                2,
                RowKind::Tool,
                StatusKind::Done,
                Some(0.0),
                Some(10.0),
                false,
            )),
            TimeDisplayRow::Row(time_row(
                "prime.review.agent.deep",
                3,
                RowKind::Tool,
                StatusKind::Done,
                Some(0.0),
                Some(5.0),
                false,
            )),
            TimeDisplayRow::Row(time_row(
                "prime.review.check",
                2,
                RowKind::Tool,
                StatusKind::Pending,
                None,
                None,
                false,
            )),
        ];
        let plan = PlanTree::empty();
        let header = base_header();
        let extra =
            build_time_zoom_extra("prime.review", &display_rows, &plan, &header, Some(20.0));
        match extra {
            Some(TimeZoomExtra::Container {
                children,
                start_s,
                axis_end_s,
            }) => {
                assert_eq!(
                    children.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
                    vec!["prime.review.agent", "prime.review.check"],
                    "only direct children, not the grandchild"
                );
                assert_eq!(start_s, 0.0);
                assert_eq!(axis_end_s, 20.0, "no duration yet, so the axis runs to now");
            }
            other => panic!("expected Container, got {other:?}"),
        }
    }

    #[test]
    fn zoom_extra_marks_a_running_tool_as_running_and_a_running_prompt_as_waiting() {
        let plan = PlanTree::empty();
        let header = base_header();
        let tool_rows = vec![TimeDisplayRow::Row(time_row(
            "prime.a",
            1,
            RowKind::Tool,
            StatusKind::Running,
            Some(5.0),
            None,
            false,
        ))];
        match build_time_zoom_extra("prime.a", &tool_rows, &plan, &header, Some(8.0)) {
            Some(TimeZoomExtra::StepBar {
                waiting_label,
                start_s,
                axis_end_s,
                ..
            }) => {
                assert_eq!(waiting_label, Some("running"));
                assert_eq!(start_s, 5.0);
                assert_eq!(axis_end_s, 8.0);
            }
            other => panic!("expected StepBar, got {other:?}"),
        }
        let prompt_rows = vec![TimeDisplayRow::Row(time_row(
            "prime.b",
            1,
            RowKind::Prompt,
            StatusKind::Running,
            Some(5.0),
            None,
            false,
        ))];
        match build_time_zoom_extra("prime.b", &prompt_rows, &plan, &header, Some(8.0)) {
            Some(TimeZoomExtra::StepBar { waiting_label, .. }) => {
                assert_eq!(waiting_label, Some("waiting for the reply"))
            }
            other => panic!("expected StepBar, got {other:?}"),
        }
    }

    #[test]
    fn zoom_extra_reads_p50_p90_from_pooled_history_for_a_finished_tool() {
        let plan = PlanTree::empty();
        let mut header = base_header();
        header.history_step_usual.insert(
            "a".to_string(),
            crate::history::StepStats {
                count: 5,
                busy_p50_s: Some(60.0),
                busy_p90_s: Some(90.0),
                ..Default::default()
            },
        );
        let rows = vec![TimeDisplayRow::Row(time_row(
            "prime.a",
            1,
            RowKind::Tool,
            StatusKind::Done,
            Some(0.0),
            Some(70.0),
            false,
        ))];
        match build_time_zoom_extra("prime.a", &rows, &plan, &header, None) {
            Some(TimeZoomExtra::StepBar {
                p50_s,
                p90_s,
                waiting_label,
                ..
            }) => {
                assert_eq!(p50_s, Some(60.0));
                assert_eq!(p90_s, Some(90.0));
                assert_eq!(waiting_label, None, "finished, not waiting or running");
            }
            other => panic!("expected StepBar, got {other:?}"),
        }
    }
}
