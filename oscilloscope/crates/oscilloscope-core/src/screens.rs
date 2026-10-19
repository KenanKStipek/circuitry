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
use crate::render::{Row, RowKind, TokenBar, details_for, token_bar_in_flight_line};
use crate::treemap::TreemapNode;

/// Which of the four screens is on display. `0` and `2` have a
/// screen to show; `1` and `3` (time, cost) don't yet, so their key
/// does nothing and they get no tab (issue #626).
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

    /// `false` for `Time`/`Cost`: they arrive in their own issues.
    pub fn is_available(self) -> bool {
        matches!(self, Screen::Overview | Screen::Flow)
    }

    /// The screen a `0`-`3` key names, or `None` for any other key,
    /// or for a digit whose screen isn't built yet (`1`, `3`) --
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

    /// Every screen with a tab, in tab order (issue #626's header:
    /// "until then they have no tab").
    pub fn available() -> &'static [Screen] {
        &[Screen::Overview, Screen::Flow]
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
    let eta_text = header
        .innermost_loop_eta_ts
        .as_deref()
        .and_then(|ts| format_clock_time(ts, zone))
        .map(|hhmmss| format!("ETA ~{}", &hhmmss[..5.min(hhmmss.len())]));
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

    // 2. The report truncates, down to a bare `▸`.
    if let Some(full) = &report_text {
        let mut n = full.chars().count();
        loop {
            let candidate = if n == 0 {
                "▸".to_string()
            } else {
                truncate_tail(full, n)
            };
            if fits(&document, Some(&candidate), eta.as_deref(), &tabs) || n == 0 {
                report = Some(candidate);
                break;
            }
            n -= 1;
        }
        if fits(&document, report.as_deref(), eta.as_deref(), &tabs) {
            return build(&document, report.as_deref(), eta.as_deref(), &tabs);
        }
    }

    // 3. The document truncates, keeping its own tail.
    {
        let mut n = original_document.chars().count();
        loop {
            let candidate = if n == 0 {
                String::new()
            } else {
                truncate_head_keep_tail(&original_document, n)
            };
            if fits(&candidate, report.as_deref(), eta.as_deref(), &tabs) || n == 0 {
                document = candidate;
                break;
            }
            n -= 1;
        }
        if fits(&document, report.as_deref(), eta.as_deref(), &tabs) {
            return build(&document, report.as_deref(), eta.as_deref(), &tabs);
        }
    }

    // 4. The ETA is dropped entirely. Status, elapsed and the clock
    // never truncate -- if the line is still too wide after this,
    // it simply overflows (the renderer clips it, same as any other
    // line that doesn't fit).
    eta = None;
    build(&document, report.as_deref(), eta.as_deref(), &tabs)
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
    /// token-bar rule). `note` is always [`AGENT_LIVE_DATA_NOTE`] --
    /// kept on the variant, not read from the constant by the front
    /// end, so a test can assert what this step actually says
    /// without reaching past the model for it.
    Agent {
        engine: Option<String>,
        model: Option<String>,
        note: &'static str,
    },
    Plain,
}

/// [`FocusDetail::Agent`]'s own note (issue #626): live agent session
/// data (a running transcript, its token usage) is a later issue
/// (#612's "live agent data"), so this is the one honest thing the
/// overview can say about an agent step today.
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct EndedSummary {
    pub outcome: RunStatus,
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub cost_usd: Option<f64>,
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
    pub top_store: Option<(String, u64)>,
    pub in_flight: String,
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

fn is_agent_row(row: &Row, flat: &BTreeMap<String, NodeMeta>) -> bool {
    row.kind == RowKind::Tool
        && node_for(row, flat)
            .and_then(|n| n.meta.get("provider"))
            .and_then(Value::as_str)
            .is_some_and(|p| p.eq_ignore_ascii_case("agent"))
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
    let since_ts = node.and_then(|n| n.created_at.clone());
    let elapsed_s = since_ts
        .as_deref()
        .and_then(|start| duration_seconds(start, now));
    let details = details_for(&row.path, plan, model, state);
    let agent = is_agent_row(row, flat);
    let detail = if agent {
        let meta = node.map(|n| &n.meta);
        FocusDetail::Agent {
            engine: meta
                .and_then(|m| m.get("engine"))
                .and_then(Value::as_str)
                .map(str::to_string),
            model: meta
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string),
            note: AGENT_LIVE_DATA_NOTE,
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
    FocusedStep {
        path: row.path.clone(),
        breadcrumb: row.label.clone(),
        kind_label: if agent { "agent" } else { row.kind.label() },
        provider_or_model: row.dim.clone(),
        since_ts,
        elapsed_s,
        detail,
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
    log: &[crate::diff::LogLine],
    now: &str,
) -> NowPane {
    const LOG_TAIL: usize = 20;
    let log_lines: Vec<String> = log
        .iter()
        .rev()
        .take(LOG_TAIL)
        .map(|l| l.text.clone())
        .rev()
        .collect();

    let focus = if !matches!(header.status, RunStatus::Running) {
        NowFocus::Ended(EndedSummary {
            outcome: header.status,
            start_ts: header.run_start_ts.clone(),
            end_ts: header.run_end_ts.clone(),
            tokens_sent: header.tokens_sent,
            tokens_received: header.tokens_received,
            cost_usd: state
                .and_then(run_totals)
                .and_then(|t| t.get("cost_usd").and_then(Value::as_f64)),
        })
    } else {
        let running: Vec<&Row> = rows
            .iter()
            .filter(|r| r.status.kind == StatusKind::Running)
            .collect();
        if let Some(&target) = running.first() {
            let step = build_focused_step(target, flat, plan, model, state, now);
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
    TimeCard {
        start_ts: header.run_start_ts.clone(),
        end_ts: header.run_end_ts.clone(),
        elapsed_s: header.elapsed_s,
        eta_ts: header.innermost_loop_eta_ts.clone(),
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
    let failures = rows
        .iter()
        .filter(|r| {
            matches!(
                r.status.kind,
                StatusKind::Failed | StatusKind::FailedHandled
            )
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
    if recent.len() < 2
        && let Some(report) = model.latest_report()
    {
        recent.push(FlowEvent::Report {
            message: report.message.clone(),
            ts: Some(report.ts.clone()),
        });
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

fn build_cost_card(
    header: &crate::render::Header,
    state: Option<&Value>,
    treemap: &[TreemapNode],
    token_bar: &TokenBar,
) -> CostCard {
    fn top(nodes: &[TreemapNode]) -> Option<(String, u64)> {
        let mut best: Option<(String, u64)> = None;
        for node in nodes {
            if node.children.is_empty()
                && best.as_ref().is_none_or(|(_, total)| node.total > *total)
            {
                best = Some((node.path.clone(), node.total));
            }
            if let Some(child_best) = top(&node.children)
                && best.as_ref().is_none_or(|(_, total)| child_best.1 > *total)
            {
                best = Some(child_best);
            }
        }
        best
    }
    CostCard {
        tokens_sent: header.tokens_sent,
        tokens_received: header.tokens_received,
        cost_usd: state
            .and_then(run_totals)
            .and_then(|t| t.get("cost_usd").and_then(Value::as_f64))
            .filter(|c| *c > 0.0),
        top_store: top(treemap),
        in_flight: token_bar_in_flight_line(token_bar),
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
    treemap: &[TreemapNode],
    log: &[crate::diff::LogLine],
    now: &str,
) -> Overview {
    let mut phase_strip = build_phase_strip(rows);
    fill_phase_start_times(&mut phase_strip, flat);
    let now_pane = build_now_pane(rows, flat, plan, model, state, header, log, now);
    let time_card = build_time_card(header, &phase_strip);
    let flow_card = build_flow_card(rows, unfolded_rows, flat, model, header);
    let cost_card = build_cost_card(header, state, treemap, token_bar);
    Overview {
        phase_strip,
        now_pane,
        time_card,
        flow_card,
        cost_card,
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
            latest_report: None,
            engine_alive: true,
        }
    }

    #[test]
    fn screen_from_key_only_accepts_available_screens() {
        assert_eq!(Screen::from_key('0'), Some(Screen::Overview));
        assert_eq!(Screen::from_key('2'), Some(Screen::Flow));
        assert_eq!(Screen::from_key('1'), None);
        assert_eq!(Screen::from_key('3'), None);
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
        assert!(text.contains("ETA ~11:58"));
        assert!(text.contains("[0 overview]"));
        assert!(text.contains("2 flow"));
        assert_eq!(text.chars().count(), segments_width(&segments));
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

    #[test]
    fn header_line_truncates_report_before_document_at_80_columns() {
        let mut header = base_header();
        header.document = "a-fairly-long-orchestration-document-name.yml".to_string();
        header.latest_report = Some(crate::render::HeaderReport {
            level: crate::observe::ReportLevel::Info,
            message: "a somewhat long report message that will not fit".to_string(),
            ts: "2026-10-08T11:52:00Z".to_string(),
        });
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
        assert!(text.contains("running"));
        assert!(text.contains("11:52:55"));
    }

    #[test]
    fn header_line_drops_eta_before_status_at_50_columns() {
        let header = base_header();
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
        assert!(text.contains("running"));
        assert!(text.contains("11:52:55"));
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
        let tmp = std::env::temp_dir().join(format!("osi-screens-test-{}", std::process::id()));
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
        // `check › taken` folds into one top-level row (single-child
        // wrapper), so the strip has two top-level rows: `plan` and
        // `check › taken`, not three.
        assert_eq!(strip.len(), 2);
        assert!(strip.iter().any(|s| s.label.contains("\u{203a}")));
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
            "2026-10-08T11:00:00Z",
        );
        match pane.focus {
            NowFocus::Running(_, more) => assert_eq!(more, 1),
            other => panic!("expected Running, got {other:?}"),
        }
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
        let no_cost = build_cost_card(
            &header,
            Some(&json!({"runtime": {"last_run": {"totals": {}}}})),
            &[],
            &token_bar,
        );
        assert_eq!(no_cost.cost_usd, None);
        let with_cost = build_cost_card(
            &header,
            Some(&json!({"runtime": {"last_run": {"totals": {"cost_usd": 14.2}}}})),
            &[],
            &token_bar,
        );
        assert_eq!(with_cost.cost_usd, Some(14.2));
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
            "2026-10-08T11:58:00Z",
        );
        match pane.focus {
            NowFocus::Ended(summary) => assert_eq!(summary.outcome, RunStatus::Failed),
            other => panic!("expected Ended, got {other:?}"),
        }
    }
}
