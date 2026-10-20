//! The Time screen's own model (issue #650, part of #612): a
//! clock-axis timeline (one row per folded step, the same rows Flow
//! already builds), ghost bars from run history, the events/min,
//! decisions and in-flight strips, the zoom pane for the selected
//! step, and the "vs usual" pane. No terminal code lives here
//! (DESIGN.md §6.1) -- `oscilloscope`'s `tui.rs` turns a `TimeRow`'s
//! own offsets into actual column positions and styling.
//!
//! Built only while screen `1` is actually on display, the same
//! performance rule `screens::build_cost_screen`/`build_flow_screen`
//! already follow.

use std::collections::BTreeMap;

use crate::history::StepStats;
use crate::model::{NodeMeta, RowStatus, RunModel, StatusKind};
use crate::observe::duration_seconds;
use crate::plan::{PlanEntryKind, PlanTree};
use crate::render::{Header, LoopProgress, Row, RowKind};
use crate::transcript::{ToolCall, ToolKind};

// ---------------------------------------------------------------------
// Bars: partial blocks, cell spans, the now column
// ---------------------------------------------------------------------

/// A bar's own trailing edge, placed within its last cell (DESIGN.md:
/// "Partial blocks (`▏`…`▉`) place each end within a cell") --
/// `fraction` is how much of that one cell the bar actually covers,
/// `0.0` excluded (a cell the bar doesn't reach at all is simply left
/// blank, never drawn through this). Only the trailing edge is
/// modelled this way; a bar's own leading edge always starts flush
/// with its first full cell, since nothing in this screen pans or
/// zooms to a sub-cell offset at either end (`+`/`-`/`=` all zoom
/// around a whole cell).
pub fn partial_block(fraction: f64) -> char {
    const GLYPHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let f = fraction.clamp(0.0, 1.0);
    let eighths = (f * 8.0).ceil().max(1.0) as usize;
    GLYPHS[eighths.min(8) - 1]
}

/// One bar's own cells on the axis: `start_col` is its first cell,
/// `full_cells` the whole cells after that one are fully covered,
/// and `partial_end` the glyph for the one cell after those where
/// the bar's own end actually falls, when that isn't a clean cell
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BarCells {
    pub start_col: usize,
    pub full_cells: usize,
    pub partial_end: Option<char>,
}

/// Places `[row_start_s, row_end_s]` (seconds from the run's own
/// start, same offset [`crate::history::StepRecord::start_s`] uses)
/// onto `width` columns spanning `[axis_start_s, axis_end_s]`.
pub fn bar_cells(
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    row_start_s: f64,
    row_end_s: f64,
) -> BarCells {
    let width = width.max(1);
    let span = (axis_end_s - axis_start_s).max(f64::MIN_POSITIVE);
    let scale = width as f64 / span;
    let start_col_f = ((row_start_s - axis_start_s) * scale).clamp(0.0, width as f64);
    let end_col_f = ((row_end_s - axis_start_s) * scale).clamp(start_col_f, width as f64);
    let start_col = start_col_f.floor() as usize;
    let end_whole = end_col_f.floor();
    let frac = end_col_f - end_whole;
    let end_whole_usize = (end_whole as usize).min(width);
    let full_cells = end_whole_usize.saturating_sub(start_col);
    let partial_end = if frac > 1e-9 && end_whole_usize < width {
        Some(partial_block(frac))
    } else {
        None
    };
    BarCells {
        start_col,
        full_cells,
        partial_end,
    }
}

/// The now line's own column (DESIGN.md: "a `┃` at the current time,
/// through every row and strip") -- `None` when `now_s` falls
/// outside the axis at all (it never has while the run is going, the
/// one case a front end actually draws it).
pub fn now_column(axis_start_s: f64, axis_end_s: f64, width: usize, now_s: f64) -> Option<usize> {
    if now_s < axis_start_s || now_s > axis_end_s {
        return None;
    }
    let width = width.max(1);
    let span = (axis_end_s - axis_start_s).max(f64::MIN_POSITIVE);
    let col = (((now_s - axis_start_s) / span) * width as f64).floor() as usize;
    Some(col.min(width - 1))
}

// ---------------------------------------------------------------------
// The clock axis's own span
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AxisSpan {
    pub start_ts: Option<String>,
    pub end_ts: String,
    /// Whether the now line is drawn at all -- `false` once the run
    /// has ended (DESIGN.md: "It isn't drawn once the run has
    /// ended").
    pub show_now: bool,
}

/// How far past `now` the axis's own end reaches while the run is
/// still going, on top of the later of `now` and the ETA's own p90
/// end (DESIGN.md: "plus a small margin") -- large enough that the
/// now line never sits flush against the pane's own right edge.
fn axis_margin_s(elapsed_s: f64) -> f64 {
    (elapsed_s * 0.05).max(30.0)
}

/// The timeline's own clock axis (DESIGN.md's "Clock axis"): start to
/// the later of `now` and the ETA's p90 end (plus a margin) while the
/// run goes, or start to end once it has one.
pub fn axis_span(header: &Header, now: &str) -> AxisSpan {
    if let Some(end_ts) = header.run_end_ts.clone() {
        return AxisSpan {
            start_ts: header.run_start_ts.clone(),
            end_ts,
            show_now: false,
        };
    }
    let now_dt = crate::diff::parse_instant(now);
    let p90_candidate = header
        .history_eta_high_ts
        .clone()
        .or_else(|| header.innermost_loop_eta_ts.clone());
    let later_ts = match (
        p90_candidate
            .as_deref()
            .and_then(crate::diff::parse_instant),
        now_dt,
    ) {
        (Some(p90), Some(now_parsed)) if p90 > now_parsed => p90_candidate.unwrap(),
        _ => now.to_string(),
    };
    let margin_s = axis_margin_s(header.elapsed_s);
    let end_ts = match crate::diff::parse_instant(&later_ts) {
        Some(parsed) => {
            (parsed + chrono::Duration::milliseconds((margin_s * 1000.0) as i64)).to_rfc3339()
        }
        None => later_ts,
    };
    AxisSpan {
        start_ts: header.run_start_ts.clone(),
        end_ts,
        show_now: true,
    }
}

// ---------------------------------------------------------------------
// Ghost bars
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostBar {
    pub start_s: f64,
    pub end_s: f64,
}

/// A pending step's own ghost bar (DESIGN.md: "each step's p50
/// `start_s`/`end_s`, shifted by how far ahead or behind the run is
/// at the ETA's anchor step") -- `None` without the step's own p50
/// start/end (below the 3-run threshold), or without a shift to
/// apply at all (`vs_usual_shift_s`, `None` exactly when there isn't
/// enough whole-run history yet, or after an edit -- DESIGN.md:
/// "There are no ghost bars without enough ok runs of the same
/// document version. After an edit, ghost bars are hidden too").
pub fn step_ghost_bar(
    stats: Option<&StepStats>,
    vs_usual_shift_s: Option<f64>,
) -> Option<GhostBar> {
    let stats = stats?;
    let shift = vs_usual_shift_s?;
    Some(GhostBar {
        start_s: stats.start_p50_s? + shift,
        end_s: stats.end_p50_s? + shift,
    })
}

/// Ghost bars' own shift (DESIGN.md: "shifted by how far ahead or
/// behind the run is") -- gated on at least
/// [`crate::history::P50_MIN_RUNS`] ok runs of the *current* document
/// hash ([`Header::history_same_hash_count`]), not directly on
/// [`Header::history_vs_usual_s`]: that field stays `None` until some
/// step has actually finished ([`crate::history::compute_eta`]'s own
/// anchor), even with plenty of same-hash runs in hand -- before any
/// step finishes, the anchor is the run's own start, so the shift at
/// that point really is zero.
pub fn vs_usual_shift_s(header: &Header) -> Option<f64> {
    let count = header.history_same_hash_count?;
    (count >= crate::history::P50_MIN_RUNS).then(|| header.history_vs_usual_s.unwrap_or(0.0))
}

/// The timeline's own `planned` row (DESIGN.md: "runs from now to the
/// ETA") -- `None` under the same conditions [`step_ghost_bar`] has
/// none, since both read the same whole-run ETA.
pub fn planned_ghost_bar(now_offset_s: f64, eta_remaining_s: Option<f64>) -> Option<GhostBar> {
    Some(GhostBar {
        start_s: now_offset_s,
        end_s: now_offset_s + eta_remaining_s?,
    })
}

// ---------------------------------------------------------------------
// Rows: the folded timeline, and loop collapse/expand
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct TimeRow {
    pub path: String,
    pub depth: usize,
    pub label: String,
    pub kind: RowKind,
    pub status: RowStatus,
    /// Run-relative seconds (same offset `StepRecord::start_s` uses)
    /// -- `None` for a row with no `created_at`/event start at all
    /// yet (pending).
    pub start_s: Option<f64>,
    pub duration_s: Option<f64>,
    pub loop_progress: Option<LoopProgress>,
    pub has_children: bool,
    /// Set for a `RowKind::Loop` row this screen's own `⏎` can expand
    /// into one row per pass -- a named loop always (its own
    /// `iter_*` children carry each pass's real span), an unnamed one
    /// only when `--events` gave its passes ids of their own
    /// (DESIGN.md: "Without `--events` they can't be told apart, and
    /// the loop stays one row").
    pub loop_expandable: bool,
    pub ghost: Option<GhostBar>,
}

/// `path`'s own start, as run-relative seconds -- `flat`'s own
/// `created_at` first, else `RunModel::event_started_at` (a
/// tree-loop pass or `use` child events alone have confirmed
/// running, DESIGN.md §1.4, which never gets a `flat` node while it
/// runs at all).
fn row_start_s(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    run_start_ts: &str,
) -> Option<f64> {
    let start_ts = flat
        .get(path)
        .and_then(|n| n.created_at.as_deref())
        .or_else(|| model.event_started_at(path))?;
    duration_seconds(run_start_ts, start_ts)
}

/// Whether `path` has at least one `iter_*` child in `flat` -- a
/// named loop's own passes, each with a real span of their own
/// (DESIGN.md §2.3); an unnamed loop's body sits directly at its own
/// parent path with no such child at all.
fn has_named_loop_passes(path: &str, flat: &BTreeMap<String, NodeMeta>) -> bool {
    let prefix = format!("{path}.iter_");
    flat.keys().any(|k| k.starts_with(&prefix))
}

/// Whether `path`'s own loop row can be expanded into one row per
/// pass (DESIGN.md's loop collapse/expand rule) -- `row_kind` is the
/// row's own [`RowKind`], since a named loop's own container row
/// (`RowKind::Loop`) and an unnamed loop's own body leaf (which
/// shares its row with the leaf underneath it, DESIGN.md §1.3: "its
/// own effective_path is exactly its enclosing scope's") need two
/// different answers.
fn loop_is_expandable(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    plan: &PlanTree,
    row_kind: RowKind,
) -> bool {
    if row_kind == RowKind::Loop {
        // A named loop with no `iter_*` child yet (no pass has
        // started) has nothing to expand into yet -- still reads as
        // expandable once it has at least one pass.
        return has_named_loop_passes(path, flat);
    }
    // Not a `RowKind::Loop` row at all -- it might still *be* an
    // unnamed loop's own body: that loop writes no path segment of
    // its own, so its own `PlanEntryKind::Loop` sits at this leaf's
    // own *parent* path (the control node's own effective path,
    // identical to its enclosing scope's), not at the leaf's.
    let Some((parent, _)) = path.rsplit_once('.') else {
        return false;
    };
    let is_unnamed_loop_body = plan.match_path(parent).is_some_and(|m| {
        m.entries
            .iter()
            .any(|e| matches!(e.kind, PlanEntryKind::Loop { named: false, .. }))
    });
    if !is_unnamed_loop_body {
        return false;
    }
    // Without `--events`, successive passes reuse the same path with
    // no id to tell them apart (DESIGN.md: "they can't be told
    // apart, and the loop stays one row").
    model.has_events() && model.step_intervals().iter().any(|i| i.path == path)
}

/// Builds the timeline's own rows (DESIGN.md: "the same folded rows
/// as Flow") from `rows` (already folded by `fold::fold_rows`, the
/// same list Flow and the overview use) -- a caller builds this only
/// while screen `1` is actually on display.
pub fn build_time_rows(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    plan: &PlanTree,
    header: &Header,
    step_usual: &BTreeMap<String, StepStats>,
) -> Vec<TimeRow> {
    let Some(run_start_ts) = header.run_start_ts.as_deref() else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| {
            let start_s = row_start_s(&row.path, flat, model, run_start_ts);
            let loop_expandable = loop_is_expandable(&row.path, flat, model, plan, row.kind);
            let ghost = if row.status.kind == StatusKind::Pending {
                let key = crate::history::display_key(&row.path, plan);
                let stats = key.as_deref().and_then(|k| step_usual.get(k));
                step_ghost_bar(stats, vs_usual_shift_s(header))
            } else {
                None
            };
            TimeRow {
                path: row.path.clone(),
                depth: row.depth,
                label: row.label.clone(),
                kind: row.kind,
                status: row.status.clone(),
                start_s,
                duration_s: row.duration_s,
                loop_progress: row.loop_progress,
                has_children: row.has_children,
                loop_expandable,
                ghost,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoopPassRow {
    /// `"pass 1"`/`"pass 2"` for a named loop (the state pane's own
    /// `iter_N` -- 1-based), `"#1"`/`"#2"` for an unnamed one by
    /// event order (DESIGN.md: "An unnamed loop's passes come from
    /// event ids").
    pub label: String,
    pub status: RowStatus,
    pub start_s: Option<f64>,
    pub duration_s: Option<f64>,
}

/// A named loop's own passes, one row per `iter_N` index found under
/// `path`, numeric order (DESIGN.md: "tree passes overlap, and that
/// is shown"). A pass's own span is the min `created_at`/max
/// `completed_at` over every flat node in its own subtree, not read
/// from `path.iter_N` itself -- a chain `each` loop's pass container
/// carries no `{value, meta}` of its own at all in either engine's
/// real state (only its children, e.g. `path.iter_0.nap`, do); a
/// tree loop's own pass container does, but aggregating either way
/// gives the same answer when it's the only node in its own subtree.
fn expand_named_loop(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    run_start_ts: &str,
    now: &str,
) -> Vec<LoopPassRow> {
    let prefix = format!("{path}.iter_");
    let mut indices: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for child_path in flat.keys() {
        if let Some(suffix) = child_path.strip_prefix(&prefix) {
            let digits: String = suffix.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(index) = digits.parse::<u32>() {
                    indices.insert(index);
                }
            }
        }
    }
    indices
        .into_iter()
        .map(|index| {
            let pass_path = format!("{path}.iter_{index}");
            let pass_prefix = format!("{pass_path}.");
            let mut start_ts: Option<&str> = None;
            let mut end_ts: Option<&str> = None;
            let mut running = false;
            let mut ok = true;
            for (child_path, node) in flat {
                if child_path != &pass_path && !child_path.starts_with(&pass_prefix) {
                    continue;
                }
                if let Some(created_at) = node.created_at.as_deref() {
                    if start_ts.is_none_or(|s| created_at < s) {
                        start_ts = Some(created_at);
                    }
                }
                match node.completed_at.as_deref() {
                    Some(completed_at) => {
                        if end_ts.is_none_or(|e| completed_at > e) {
                            end_ts = Some(completed_at);
                        }
                        ok &= node.error.is_none();
                    }
                    None => running = true,
                }
            }
            let start_s = start_ts.and_then(|t| duration_seconds(run_start_ts, t));
            let end_s = if running {
                duration_seconds(run_start_ts, now)
            } else {
                end_ts.and_then(|t| duration_seconds(run_start_ts, t))
            };
            let duration_s = start_s.zip(end_s).map(|(s, e)| e - s);
            let status = RowStatus {
                kind: if running {
                    StatusKind::Running
                } else if ok {
                    StatusKind::Done
                } else {
                    StatusKind::Failed
                },
                skip_reason: None,
                retrying: false,
            };
            LoopPassRow {
                label: format!("pass {}", index + 1),
                status,
                start_s,
                duration_s,
            }
        })
        .collect()
}

/// An unnamed loop's own passes, by event id order (DESIGN.md: "An
/// unnamed loop's passes come from event ids (`#1`, `#2`, …)") --
/// empty when `--events` never gave this path any (the caller's own
/// [`loop_is_expandable`] check keeps this from ever being asked to,
/// but an empty result is itself the right answer either way).
fn expand_unnamed_loop(
    path: &str,
    model: &RunModel,
    run_start_ts: &str,
    now: &str,
) -> Vec<LoopPassRow> {
    model
        .step_intervals()
        .iter()
        .filter(|i| i.path == path)
        .enumerate()
        .map(|(n, interval)| {
            let start_s = duration_seconds(run_start_ts, &interval.start_ts);
            let duration_s = duration_seconds(&interval.start_ts, &interval.end_ts);
            let _ = now;
            LoopPassRow {
                label: format!("#{}", n + 1),
                status: RowStatus {
                    kind: if interval.ok {
                        StatusKind::Done
                    } else {
                        StatusKind::Failed
                    },
                    skip_reason: None,
                    retrying: false,
                },
                start_s,
                duration_s,
            }
        })
        .collect()
}

/// Expands `path`'s own loop row into one row per pass (DESIGN.md:
/// "`⏎` on a loop row expands it into one row per pass") -- empty
/// when [`loop_is_expandable`] (checked by the caller via
/// `TimeRow::loop_expandable`) would have said `false`.
pub fn expand_loop(
    path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    run_start_ts: &str,
    now: &str,
) -> Vec<LoopPassRow> {
    if has_named_loop_passes(path, flat) {
        expand_named_loop(path, flat, run_start_ts, now)
    } else {
        expand_unnamed_loop(path, model, run_start_ts, now)
    }
}

// ---------------------------------------------------------------------
// Strips
// ---------------------------------------------------------------------

/// Buckets `points` (each an offset in run-relative seconds, with its
/// own weight -- an ordinary event's weight is `1`; the interval
/// log's own per-minute overflow tail folds in at a larger weight
/// from one representative point per minute) onto `width` columns
/// spanning `[axis_start_s, axis_end_s]`. The one bucketing rule
/// every strip in this module shares.
pub fn bucket_weighted(
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    points: &[(f64, u64)],
) -> Vec<u64> {
    let width = width.max(1);
    let mut heights = vec![0u64; width];
    let span = (axis_end_s - axis_start_s).max(f64::MIN_POSITIVE);
    for &(t, weight) in points {
        if t < axis_start_s || t > axis_end_s {
            continue;
        }
        let col = (((t - axis_start_s) / span) * width as f64).floor() as usize;
        heights[col.min(width - 1)] += weight;
    }
    heights
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventsPerMinuteStrip {
    pub heights: Vec<u64>,
    /// Set without `--events` (DESIGN.md: "osi counts state's
    /// `created_at`/`completed_at` instead and labels the strip
    /// `(from state)`").
    pub from_state: bool,
}

/// The events/min strip's own heights (DESIGN.md): one start event
/// and one end event per logged interval, each at its own offset,
/// plus the interval log's own per-minute overflow tail folded in at
/// the minute's own midpoint.
pub fn events_per_minute_strip(
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    model: &RunModel,
    run_start_ts: &str,
) -> EventsPerMinuteStrip {
    EventsPerMinuteStrip {
        heights: bucket_weighted(
            axis_start_s,
            axis_end_s,
            width,
            &event_points(model, run_start_ts, &[]),
        ),
        from_state: !model.has_events(),
    }
}

/// The events/min strip's own raw points, unbucketed: a drawing front
/// end re-buckets these at whatever width and visible window the
/// bars themselves are drawn at right now (issue #650's own axis
/// zoom), rather than at a separate, fixed resolution that can't
/// line up with them. One point per logged interval's own start and
/// end, plus the interval log's own per-minute overflow tail folded
/// in at the minute's own midpoint, plus `running_starts` -- a
/// currently running step's own start (DESIGN.md: "start and end
/// events per minute"), which [`RunModel::step_intervals`] never
/// carries at all until the step actually finishes.
pub fn event_points(
    model: &RunModel,
    run_start_ts: &str,
    running_starts: &[f64],
) -> Vec<(f64, u64)> {
    let mut points: Vec<(f64, u64)> = Vec::new();
    for interval in model.step_intervals() {
        if let Some(s) = duration_seconds(run_start_ts, &interval.start_ts) {
            points.push((s, 1));
        }
        if let Some(e) = duration_seconds(run_start_ts, &interval.end_ts) {
            points.push((e, 1));
        }
    }
    for &s in running_starts {
        points.push((s, 1));
    }
    for (minute, count) in model.overflowed_minute_counts() {
        // `minute` is `"2026-01-01T00:05"` -- `:30Z` places the
        // representative point at its own midpoint, close enough at
        // the per-minute granularity the cap already dropped to.
        let ts = format!("{minute}:30Z");
        if let Some(s) = duration_seconds(run_start_ts, &ts) {
            points.push((s, u64::from(*count)));
        }
    }
    points
}

/// "events / min ... quiet = agent" (DESIGN.md) -- shown when the
/// only step running right now is an agent, and the strip's own rate
/// has actually dropped (`recent_rate`, the caller's own count of
/// start/end events in some short trailing window), not merely
/// because an agent happens to be running.
pub fn quiet_agent(running_leaf_kinds: &[RowKind], recent_rate: u64) -> bool {
    running_leaf_kinds == [RowKind::Prompt] && recent_rate == 0
}

/// The clock axis's own tick columns (DESIGN.md: "labelled ticks at
/// round clock times ... whichever keeps labels about 10 columns
/// apart") -- one entry per tick, each paired with its own
/// run-relative offset for a caller to format into an actual clock
/// time and place on the same `width` columns the bars themselves
/// use.
pub fn axis_tick_columns(axis_start_s: f64, axis_end_s: f64, width: usize) -> Vec<(usize, f64)> {
    let width = width.max(1);
    let span = (axis_end_s - axis_start_s).max(0.0);
    let tick_s = crate::format::clock_tick_seconds(span, width, 10);
    if tick_s <= 0.0 {
        return Vec::new();
    }
    let first_tick = (axis_start_s / tick_s).ceil() * tick_s;
    let mut out = Vec::new();
    let mut t = first_tick;
    while t <= axis_end_s + 1e-9 {
        if let Some(col) = now_column(axis_start_s, axis_end_s, width, t) {
            out.push((col, t));
        }
        t += tick_s;
    }
    out
}

/// The decisions strip's own marks (DESIGN.md: "`◆` at the time each
/// named `if` decided"), from the story log's own `Decision` lines --
/// the first sighting of each path (oldest first, same order
/// `story_lines` is already in), since a retried pass's `if` can
/// decide more than once.
pub fn decision_points(story_lines: &[crate::diff::LogLine], run_start_ts: &str) -> Vec<f64> {
    let mut seen = std::collections::BTreeSet::new();
    let mut points = Vec::new();
    for line in story_lines {
        if line.kind != crate::diff::LogKind::Decision {
            continue;
        }
        let Some(path) = &line.path else { continue };
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Some(ts) = &line.ts {
            if let Some(s) = duration_seconds(run_start_ts, ts) {
                points.push(s);
            }
        }
    }
    points
}

/// The in-flight strip's own heights (DESIGN.md: "how many steps are
/// running at once") -- a sweep over every row's own `[start_s,
/// end_s]` (running rows extended to `now_s`), counted per column the
/// same way [`bucket_weighted`]'s callers are, but by overlap rather
/// than by point.
pub fn in_flight_heights(
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    intervals: &[(f64, f64)],
) -> Vec<u64> {
    let width = width.max(1);
    let span = (axis_end_s - axis_start_s).max(f64::MIN_POSITIVE);
    let mut heights = vec![0u64; width];
    for &(start, end) in intervals {
        let start_col = (((start.max(axis_start_s) - axis_start_s) / span) * width as f64)
            .floor()
            .max(0.0) as usize;
        let end_col =
            (((end.min(axis_end_s) - axis_start_s) / span) * width as f64).ceil() as usize;
        let end_col = end_col.clamp(start_col + 1, width);
        for height in &mut heights[start_col.min(width - 1)..end_col] {
            *height += 1;
        }
    }
    heights
}

/// How many running steps are currently queued behind a concurrency
/// slot right now (DESIGN.md: "`N queued` when any prompt is waiting
/// for a concurrency slot (`meta.waiting_for`)") -- a snapshot, not a
/// strip height, since only the present moment is ever shown.
pub fn queued_now(flat: &BTreeMap<String, NodeMeta>) -> u64 {
    flat.values()
        .filter(|n| n.is_running() && n.meta.get("waiting_for").is_some_and(|v| !v.is_null()))
        .count() as u64
}

// ---------------------------------------------------------------------
// The zoom pane
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomBandKind {
    Read,
    Edit,
    Bash,
    Other,
    /// The session's own time outside any tool call (DESIGN.md: "A
    /// `think` band covers the session's time outside any tool
    /// call").
    Think,
}

impl From<ToolKind> for ZoomBandKind {
    fn from(kind: ToolKind) -> Self {
        match kind {
            ToolKind::Read => ZoomBandKind::Read,
            ToolKind::Edit => ZoomBandKind::Edit,
            ToolKind::Bash => ZoomBandKind::Bash,
            ToolKind::Other => ZoomBandKind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomBand {
    pub kind: ZoomBandKind,
    pub start_s: f64,
    pub end_s: f64,
    /// A call still running (DESIGN.md: "A call still running
    /// extends to now").
    pub running: bool,
}

/// An agent's own tool bands plus `think` bands, from its transcript
/// (DESIGN.md's zoom pane: "one band per kind of tool ... from the
/// transcript's `tool_start`/`tool_end` times"). `calls` is already
/// oldest-first (`TranscriptView::calls`'s own order); `session_
/// start_s`/`now_s` are run-relative seconds. A call missing its own
/// `start_t` (shouldn't happen -- every parsed call has one) is
/// skipped rather than guessed at.
pub fn agent_zoom_bands(
    calls: &[ToolCall],
    session_start_s: f64,
    now_s: f64,
    run_start_ts: &str,
    agent_running: bool,
) -> Vec<ZoomBand> {
    let mut tool_bands: Vec<ZoomBand> = calls
        .iter()
        .filter_map(|call| {
            let start_s = duration_seconds(run_start_ts, call.start_t.as_deref()?)?;
            let (end_s, running) = match &call.end_t {
                Some(end_t) => (
                    duration_seconds(run_start_ts, end_t).unwrap_or(now_s),
                    false,
                ),
                None => (now_s, call.is_running()),
            };
            Some(ZoomBand {
                kind: ToolKind::classify(&call.tool).into(),
                start_s,
                end_s,
                running,
            })
        })
        .collect();
    tool_bands.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));

    let mut bands = Vec::with_capacity(tool_bands.len() * 2 + 1);
    let mut cursor = session_start_s;
    for band in &tool_bands {
        if band.start_s > cursor {
            bands.push(ZoomBand {
                kind: ZoomBandKind::Think,
                start_s: cursor,
                end_s: band.start_s,
                running: false,
            });
        }
        cursor = cursor.max(band.end_s);
        bands.push(*band);
    }
    if agent_running && now_s > cursor {
        bands.push(ZoomBand {
            kind: ZoomBandKind::Think,
            start_s: cursor,
            end_s: now_s,
            running: true,
        });
    }
    bands
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ZoomAgentFooter {
    pub turns: Option<u64>,
    pub tool_calls: Option<u64>,
    /// The busiest tool by its own time spent (DESIGN.md: "`bash
    /// 4m10s (67%)`") -- name, milliseconds, and that figure as a
    /// percentage of the session's own elapsed time so far.
    pub busiest_tool: Option<(String, u64, f64)>,
}

/// [`ZoomAgentFooter`], from either `meta.live`'s own `turns`/`tool_
/// calls`/`tools.<name>.ms` while running, or `meta.raw`'s once
/// finished (DESIGN.md: "from `meta.live`'s `turns`, `tool_calls` and
/// `tools.<name>.ms` against the session's elapsed time, or
/// `meta.raw`'s `turns`, `tool_calls` and `tool_counts` once it has
/// finished").
pub fn zoom_agent_footer(
    turns: Option<u64>,
    tool_calls: Option<u64>,
    tools: &BTreeMap<String, crate::model::AgentLiveToolUsage>,
    elapsed_s: f64,
) -> ZoomAgentFooter {
    let busiest_tool = tools
        .iter()
        .max_by_key(|(_, usage)| usage.ms)
        .map(|(name, usage)| {
            let pct = if elapsed_s > 0.0 {
                (usage.ms as f64 / 1000.0 / elapsed_s * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            (name.clone(), usage.ms, pct)
        });
    ZoomAgentFooter {
        turns,
        tool_calls,
        busiest_tool,
    }
}

// ---------------------------------------------------------------------
// "vs usual"
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsUsualMark {
    Slower,
    Faster,
    Usual,
}

/// DESIGN.md's own marks rule: outside p10/p90 with 5 or more runs,
/// else more than 20% from p50 with 3 or 4 -- `running` keeps a step
/// still in progress from ever reading `▼` (faster), since its own
/// `now_s` is only how far it has *gotten*, not how long it took: it
/// reads `≈` until it either finishes or passes the upper bound.
pub fn vs_usual_mark(now_s: f64, stats: &StepStats, running: bool) -> Option<VsUsualMark> {
    let p50 = stats.busy_p50_s?;
    if stats.count >= 5 {
        let p10 = stats.busy_p10_s?;
        let p90 = stats.busy_p90_s?;
        return Some(if now_s > p90 {
            VsUsualMark::Slower
        } else if !running && now_s < p10 {
            VsUsualMark::Faster
        } else {
            VsUsualMark::Usual
        });
    }
    if stats.count >= 3 {
        let delta = (now_s - p50) / p50.max(f64::MIN_POSITIVE);
        return Some(if delta > 0.2 {
            VsUsualMark::Slower
        } else if !running && delta < -0.2 {
            VsUsualMark::Faster
        } else {
            VsUsualMark::Usual
        });
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct VsUsualRow {
    pub label: String,
    pub now_s: f64,
    pub running: bool,
    pub p50_s: f64,
    pub mark: VsUsualMark,
}

/// "earlier versions" (DESIGN.md: "After an edit, the title reads
/// `vs usual · earlier versions`") -- below
/// [`crate::history::P50_MIN_RUNS`] ok runs of the *current* document
/// hash, but with pooled step history from other hashes to show
/// instead (`Header::history_step_usual`'s own "earlier versions"
/// pooling, issue #649). Not keyed on
/// [`crate::render::Header::history_vs_usual_s`]: that field is also
/// `None` before any step has finished even with plenty of same-hash
/// runs ([`vs_usual_shift_s`]'s own doc comment), and it is `None`
/// with history off entirely, which must show no title suffix at all
/// -- both read `false` here instead.
pub fn vs_usual_is_earlier_versions(header: &Header) -> bool {
    header
        .history_same_hash_count
        .is_some_and(|count| count < crate::history::P50_MIN_RUNS)
        && !header.history_step_usual.is_empty()
}

/// Issue #650's own "too few runs" case (DESIGN.md: "`history: 1 of 3
/// runs of this version`") -- `Some((count, threshold))` below
/// [`crate::history::P50_MIN_RUNS`] same-hash ok runs, but only when
/// there is no pooled step history to fall back on at all
/// (`Header::history_step_usual`'s empty): with some, the "earlier
/// versions" rows show instead, even for a same-hash count of zero
/// right after an edit.
pub fn vs_usual_too_few_runs(header: &Header) -> Option<(usize, usize)> {
    if !header.history_step_usual.is_empty() {
        return None;
    }
    let count = header.history_same_hash_count?;
    (count < crate::history::P50_MIN_RUNS).then_some((count, crate::history::P50_MIN_RUNS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_block_covers_every_eighth() {
        assert_eq!(partial_block(0.0), '▏');
        assert_eq!(partial_block(0.01), '▏');
        assert_eq!(partial_block(0.125), '▏');
        assert_eq!(partial_block(0.2), '▎');
        assert_eq!(partial_block(0.5), '▌');
        assert_eq!(partial_block(0.99), '█');
        assert_eq!(partial_block(1.0), '█');
    }

    #[test]
    fn bar_cells_places_a_bar_fully_inside_the_axis() {
        // A 100s axis over 10 columns: a bar from 20s to 55s covers
        // columns 2..5 fully, with a half cell at column 5.
        let cells = bar_cells(0.0, 100.0, 10, 20.0, 55.0);
        assert_eq!(cells.start_col, 2);
        assert_eq!(cells.full_cells, 3);
        assert_eq!(cells.partial_end, Some('▌'));
    }

    #[test]
    fn bar_cells_has_no_partial_end_on_an_exact_cell_boundary() {
        let cells = bar_cells(0.0, 100.0, 10, 0.0, 50.0);
        assert_eq!(cells.start_col, 0);
        assert_eq!(cells.full_cells, 5);
        assert_eq!(cells.partial_end, None);
    }

    #[test]
    fn bar_cells_clamps_a_running_bar_to_the_axis_end() {
        let cells = bar_cells(0.0, 100.0, 10, 90.0, 500.0);
        assert_eq!(cells.start_col, 9);
        assert_eq!(cells.full_cells, 1);
        assert_eq!(cells.partial_end, None);
    }

    #[test]
    fn now_column_is_none_outside_the_axis() {
        assert_eq!(now_column(0.0, 100.0, 10, -1.0), None);
        assert_eq!(now_column(0.0, 100.0, 10, 101.0), None);
        assert_eq!(now_column(0.0, 100.0, 10, 0.0), Some(0));
        assert_eq!(now_column(0.0, 100.0, 10, 99.9), Some(9));
    }

    fn header_with(run_start: Option<&str>, run_end: Option<&str>) -> Header {
        Header {
            document: "do.yml".to_string(),
            engine: "cof".to_string(),
            run_id: None,
            status: crate::model::RunStatus::Running,
            elapsed_s: 300.0,
            run_start_ts: run_start.map(str::to_string),
            run_end_ts: run_end.map(str::to_string),
            effects_done: 0,
            effects_planned: None,
            effects_planned_is_lower_bound: false,
            tokens_sent: 0,
            tokens_received: 0,
            innermost_loop_eta_s: None,
            innermost_loop_eta_ts: None,
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
    fn axis_span_runs_start_to_end_once_the_run_has_ended() {
        let header = header_with(Some("2026-01-01T00:00:00Z"), Some("2026-01-01T00:10:00Z"));
        let span = axis_span(&header, "2026-01-01T00:20:00Z");
        assert_eq!(span.start_ts.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(span.end_ts, "2026-01-01T00:10:00Z");
        assert!(!span.show_now);
    }

    #[test]
    fn axis_span_reaches_past_now_while_running_with_no_eta() {
        let header = header_with(Some("2026-01-01T00:00:00Z"), None);
        let now = "2026-01-01T00:05:00Z";
        let span = axis_span(&header, now);
        assert!(span.show_now);
        let now_dt = crate::diff::parse_instant(now).unwrap();
        let end_dt = crate::diff::parse_instant(&span.end_ts).unwrap();
        assert!(end_dt > now_dt);
    }

    #[test]
    fn axis_span_reaches_the_eta_p90_when_its_later_than_now() {
        let mut header = header_with(Some("2026-01-01T00:00:00Z"), None);
        header.history_eta_high_ts = Some("2026-01-01T00:30:00Z".to_string());
        let span = axis_span(&header, "2026-01-01T00:05:00Z");
        let p90_dt = crate::diff::parse_instant("2026-01-01T00:30:00Z").unwrap();
        let end_dt = crate::diff::parse_instant(&span.end_ts).unwrap();
        assert!(end_dt > p90_dt);
    }

    fn stats_with(count: usize, p50: f64, start_p50: f64, end_p50: f64) -> StepStats {
        StepStats {
            count,
            busy_p50_s: Some(p50),
            start_p50_s: Some(start_p50),
            end_p50_s: Some(end_p50),
            ..Default::default()
        }
    }

    #[test]
    fn ghost_bar_shifts_by_how_far_behind_the_run_is() {
        let stats = stats_with(5, 10.0, 100.0, 110.0);
        let ghost = step_ghost_bar(Some(&stats), Some(20.0)).unwrap();
        assert_eq!(ghost.start_s, 120.0);
        assert_eq!(ghost.end_s, 130.0);
    }

    #[test]
    fn ghost_bar_is_none_without_a_shift_after_an_edit() {
        let stats = stats_with(5, 10.0, 100.0, 110.0);
        assert_eq!(step_ghost_bar(Some(&stats), None), None);
    }

    #[test]
    fn ghost_bar_is_none_below_the_run_threshold() {
        let stats = StepStats {
            count: 2,
            ..Default::default()
        };
        assert_eq!(step_ghost_bar(Some(&stats), Some(0.0)), None);
    }

    #[test]
    fn planned_ghost_bar_runs_from_now_to_the_eta() {
        let ghost = planned_ghost_bar(300.0, Some(120.0)).unwrap();
        assert_eq!(ghost.start_s, 300.0);
        assert_eq!(ghost.end_s, 420.0);
        assert_eq!(planned_ghost_bar(300.0, None), None);
    }

    #[test]
    fn events_per_minute_strip_counts_both_ends_of_an_events_sourced_interval() {
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:10Z".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:40Z".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
            ok: true,
            ms: Some(30_000),
            error: None,
        });
        let strip = events_per_minute_strip(0.0, 60.0, 6, &model, "2026-01-01T00:00:00Z");
        assert!(!strip.from_state);
        assert_eq!(strip.heights.iter().sum::<u64>(), 2);
        assert_eq!(strip.heights[1], 1);
        assert_eq!(strip.heights[4], 1);
    }

    #[test]
    fn event_points_includes_a_running_steps_own_start() {
        let mut model = RunModel::new();
        model.observe_event(&crate::observe::Event::Start {
            ts: "2026-01-01T00:00:10Z".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
        });
        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:40Z".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
            ok: true,
            ms: Some(30_000),
            error: None,
        });
        // Finished interval contributes 2 points (start + end); the
        // currently running step's own start is a third, read from
        // `running_starts` since `step_intervals` has no entry for
        // it until it finishes.
        let points = event_points(&model, "2026-01-01T00:00:00Z", &[55.0]);
        assert_eq!(points.len(), 3);
        assert!(points.contains(&(55.0, 1)));
    }

    #[test]
    fn quiet_agent_is_true_only_for_a_lone_quiet_running_prompt() {
        assert!(quiet_agent(&[RowKind::Prompt], 0));
        assert!(
            !quiet_agent(&[RowKind::Prompt], 2),
            "the rate must be low too"
        );
        assert!(!quiet_agent(&[RowKind::Tool], 0), "a tool isn't an agent");
        assert!(!quiet_agent(&[RowKind::Prompt, RowKind::Tool], 0));
        assert!(!quiet_agent(&[], 0));
    }

    #[test]
    fn axis_tick_columns_keeps_ticks_about_10_columns_apart() {
        let ticks = axis_tick_columns(0.0, 600.0, 60);
        assert!(ticks.len() >= 2);
        for window in ticks.windows(2) {
            assert!(window[1].0 - window[0].0 >= 9);
        }
        assert_eq!(ticks[0].1 % 60.0, 0.0);
    }

    #[test]
    fn axis_tick_columns_is_empty_for_a_zero_span() {
        assert_eq!(axis_tick_columns(10.0, 10.0, 40), Vec::new());
    }

    #[test]
    fn events_per_minute_strip_without_events_reads_from_state() {
        let model = RunModel::new();
        let strip = events_per_minute_strip(0.0, 60.0, 6, &model, "2026-01-01T00:00:00Z");
        assert!(strip.from_state);
    }

    #[test]
    fn events_per_minute_strip_folds_in_the_overflow_tail() {
        let mut model = RunModel::new();
        for i in 0..(crate::model::MAX_STEP_INTERVALS + 1) {
            let id = i as i64 + 1;
            model.observe_event(&crate::observe::Event::Start {
                ts: "2026-01-01T00:00:00Z".to_string(),
                id: Some(id),
                path: "prime.loop.nap".to_string(),
            });
            model.observe_event(&crate::observe::Event::End {
                ts: "2026-01-01T00:00:00Z".to_string(),
                id: Some(id),
                path: "prime.loop.nap".to_string(),
                ok: true,
                ms: Some(0),
                error: None,
            });
        }
        let strip = events_per_minute_strip(0.0, 60.0, 1, &model, "2026-01-01T00:00:00Z");
        assert!(strip.heights[0] > 0);
    }

    #[test]
    fn decision_points_keeps_only_the_first_sighting_per_path() {
        let lines = vec![
            crate::diff::LogLine {
                ts: Some("2026-01-01T00:00:10Z".to_string()),
                text: "d1".to_string(),
                kind: crate::diff::LogKind::Decision,
                path: Some("prime.gate".to_string()),
            },
            crate::diff::LogLine {
                ts: Some("2026-01-01T00:00:20Z".to_string()),
                text: "d1 retry".to_string(),
                kind: crate::diff::LogKind::Decision,
                path: Some("prime.gate".to_string()),
            },
            crate::diff::LogLine {
                ts: Some("2026-01-01T00:00:15Z".to_string()),
                text: "other".to_string(),
                kind: crate::diff::LogKind::Start,
                path: Some("prime.gate".to_string()),
            },
        ];
        let points = decision_points(&lines, "2026-01-01T00:00:00Z");
        assert_eq!(points, vec![10.0]);
    }

    #[test]
    fn in_flight_heights_counts_overlapping_rows_per_column() {
        let intervals = vec![(0.0, 30.0), (20.0, 60.0)];
        let heights = in_flight_heights(0.0, 60.0, 6, &intervals);
        // Columns 0-9s and 10-19s: only the first row (height 1).
        assert_eq!(heights[0], 1);
        // Columns 20-29s: both rows overlap (height 2).
        assert_eq!(heights[2], 2);
        // Columns 50-59s: only the second row.
        assert_eq!(heights[5], 1);
    }

    #[test]
    fn queued_now_counts_only_running_nodes_waiting_for_a_slot() {
        use serde_json::json;
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"completed_at": null, "waiting_for": "concurrency"}},
            "b": {"value": null, "meta": {"completed_at": null, "waiting_for": null}},
            "c": {"value": "done", "meta": {"completed_at": "t", "waiting_for": "concurrency"}}
        }});
        let flat = crate::model::flatten_state(&state);
        assert_eq!(queued_now(&flat), 1);
    }

    #[test]
    fn vs_usual_mark_at_5_runs_uses_p10_p90() {
        let stats = StepStats {
            count: 5,
            busy_p50_s: Some(100.0),
            busy_p10_s: Some(80.0),
            busy_p90_s: Some(140.0),
            ..Default::default()
        };
        assert_eq!(
            vs_usual_mark(150.0, &stats, false),
            Some(VsUsualMark::Slower)
        );
        assert_eq!(
            vs_usual_mark(70.0, &stats, false),
            Some(VsUsualMark::Faster)
        );
        assert_eq!(
            vs_usual_mark(100.0, &stats, false),
            Some(VsUsualMark::Usual)
        );
    }

    #[test]
    fn vs_usual_mark_at_3_runs_uses_20_percent_of_p50() {
        let stats = StepStats {
            count: 3,
            busy_p50_s: Some(100.0),
            ..Default::default()
        };
        assert_eq!(
            vs_usual_mark(130.0, &stats, false),
            Some(VsUsualMark::Slower)
        );
        assert_eq!(
            vs_usual_mark(70.0, &stats, false),
            Some(VsUsualMark::Faster)
        );
        assert_eq!(
            vs_usual_mark(110.0, &stats, false),
            Some(VsUsualMark::Usual)
        );
    }

    #[test]
    fn vs_usual_mark_is_none_below_3_runs() {
        let stats = StepStats {
            count: 2,
            busy_p50_s: Some(100.0),
            ..Default::default()
        };
        assert_eq!(vs_usual_mark(130.0, &stats, false), None);
    }

    #[test]
    fn vs_usual_mark_keeps_a_running_step_from_reading_faster_too_soon() {
        let stats = StepStats {
            count: 5,
            busy_p50_s: Some(100.0),
            busy_p10_s: Some(80.0),
            busy_p90_s: Some(140.0),
            ..Default::default()
        };
        // Below p10 but still running: `≈`, not `▼` -- it hasn't
        // actually finished faster than usual, it just hasn't gotten
        // there yet.
        assert_eq!(vs_usual_mark(70.0, &stats, true), Some(VsUsualMark::Usual));
        // Past p90 while still running is genuinely slower already.
        assert_eq!(
            vs_usual_mark(150.0, &stats, true),
            Some(VsUsualMark::Slower)
        );
    }

    fn stats_usual(busy_p50: f64) -> StepStats {
        StepStats {
            count: 5,
            busy_p50_s: Some(busy_p50),
            ..Default::default()
        }
    }

    #[test]
    fn vs_usual_is_earlier_versions_below_the_threshold_with_pooled_history() {
        let mut header = header_with(Some("t0"), None);
        header.history_same_hash_count = Some(1);
        header
            .history_step_usual
            .insert("plan".to_string(), stats_usual(10.0));
        assert!(vs_usual_is_earlier_versions(&header));
    }

    #[test]
    fn vs_usual_is_earlier_versions_is_false_with_enough_same_hash_runs() {
        let mut header = header_with(Some("t0"), None);
        header.history_same_hash_count = Some(3);
        header
            .history_step_usual
            .insert("plan".to_string(), stats_usual(10.0));
        assert!(!vs_usual_is_earlier_versions(&header));
    }

    #[test]
    fn vs_usual_is_earlier_versions_is_false_with_history_off() {
        let header = header_with(Some("t0"), None);
        assert_eq!(header.history_same_hash_count, None);
        assert!(!vs_usual_is_earlier_versions(&header));
    }

    #[test]
    fn vs_usual_too_few_runs_reads_below_the_threshold_with_no_pooled_history() {
        let mut header = header_with(Some("t0"), None);
        header.history_same_hash_count = Some(1);
        assert_eq!(vs_usual_too_few_runs(&header), Some((1, 3)));
        header.history_same_hash_count = Some(3);
        assert_eq!(vs_usual_too_few_runs(&header), None);
    }

    #[test]
    fn vs_usual_too_few_runs_is_none_right_after_an_edit_with_pooled_history() {
        let mut header = header_with(Some("t0"), None);
        header.history_same_hash_count = Some(0);
        header
            .history_step_usual
            .insert("plan".to_string(), stats_usual(10.0));
        assert_eq!(vs_usual_too_few_runs(&header), None);
    }

    #[test]
    fn vs_usual_shift_s_is_zero_before_any_step_has_finished() {
        let mut header = header_with(Some("t0"), None);
        header.history_same_hash_count = Some(5);
        assert_eq!(header.history_vs_usual_s, None);
        assert_eq!(vs_usual_shift_s(&header), Some(0.0));
    }

    #[test]
    fn vs_usual_shift_s_is_none_below_the_same_hash_threshold() {
        let mut header = header_with(Some("t0"), None);
        header.history_same_hash_count = Some(1);
        header.history_vs_usual_s = Some(20.0);
        assert_eq!(vs_usual_shift_s(&header), None);
    }

    #[test]
    fn zoom_agent_footer_picks_the_busiest_tool_by_ms() {
        let mut tools = BTreeMap::new();
        tools.insert(
            "bash".to_string(),
            crate::model::AgentLiveToolUsage {
                calls: 4,
                ms: 250_000,
            },
        );
        tools.insert(
            "edit".to_string(),
            crate::model::AgentLiveToolUsage {
                calls: 2,
                ms: 10_000,
            },
        );
        let footer = zoom_agent_footer(Some(14), Some(31), &tools, 375.0);
        assert_eq!(footer.turns, Some(14));
        assert_eq!(footer.tool_calls, Some(31));
        let (name, ms, pct) = footer.busiest_tool.unwrap();
        assert_eq!(name, "bash");
        assert_eq!(ms, 250_000);
        assert!((pct - 66.67).abs() < 0.1);
    }

    #[test]
    fn agent_zoom_bands_covers_gaps_with_think_and_extends_a_running_call_to_now() {
        let calls = vec![
            ToolCall {
                start_t: Some("2026-01-01T00:00:00Z".to_string()),
                end_t: Some("2026-01-01T00:00:10Z".to_string()),
                tool: "read".to_string(),
                target: None,
                ok: Some(true),
                ms: Some(10_000),
                lines_added: None,
                lines_removed: None,
                stopped: false,
            },
            ToolCall {
                start_t: Some("2026-01-01T00:00:20Z".to_string()),
                end_t: None,
                tool: "bash".to_string(),
                target: None,
                ok: None,
                ms: None,
                lines_added: None,
                lines_removed: None,
                stopped: false,
            },
        ];
        let bands = agent_zoom_bands(&calls, 0.0, 30.0, "2026-01-01T00:00:00Z", true);
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[0].kind, ZoomBandKind::Read);
        assert_eq!(bands[1].kind, ZoomBandKind::Think);
        assert_eq!(bands[1].start_s, 10.0);
        assert_eq!(bands[1].end_s, 20.0);
        assert_eq!(bands[2].kind, ZoomBandKind::Bash);
        assert_eq!(bands[2].end_s, 30.0);
        assert!(bands[2].running);
    }

    #[test]
    fn agent_zoom_bands_with_no_calls_is_all_think_while_running() {
        let bands = agent_zoom_bands(&[], 0.0, 15.0, "2026-01-01T00:00:00Z", true);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].kind, ZoomBandKind::Think);
        assert_eq!(bands[0].start_s, 0.0);
        assert_eq!(bands[0].end_s, 15.0);
    }

    fn loop_row(path: &str) -> Row {
        Row {
            path: path.to_string(),
            depth: 1,
            label: "loop".to_string(),
            kind: RowKind::Loop,
            status: RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: true,
        }
    }

    #[test]
    fn a_named_chain_loops_passes_expand_from_their_own_iter_children() {
        use serde_json::json;
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "items": {"value": null, "meta": {"completed_at": null},
                "iter_0": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"}},
                "iter_1": {"value": null, "meta": {"created_at": "2026-01-01T00:00:10Z", "completed_at": "2026-01-01T00:00:25Z"}}
            }
        }});
        let flat = crate::model::flatten_state(&state);
        let model = RunModel::new();
        let plan = PlanTree::empty();
        let header = header_with(Some("2026-01-01T00:00:00Z"), None);
        let rows = vec![loop_row("prime.items")];
        let time_rows = build_time_rows(&rows, &flat, &model, &plan, &header, &BTreeMap::new());
        assert!(time_rows[0].loop_expandable);

        let passes = expand_loop(
            "prime.items",
            &flat,
            &model,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:25Z",
        );
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].label, "pass 1");
        assert_eq!(passes[0].start_s, Some(0.0));
        assert_eq!(passes[0].duration_s, Some(10.0));
        assert_eq!(passes[1].label, "pass 2");
        assert_eq!(passes[1].start_s, Some(10.0));
        assert_eq!(passes[1].duration_s, Some(15.0));
    }

    /// A chain `each` loop's own `iter_N` carries no `{value, meta}`
    /// of its own in either engine's real state -- only its own
    /// children (here, a single `nap` leaf) do. A pass's own span
    /// still comes through, aggregated from that subtree.
    #[test]
    fn a_named_chain_loop_with_no_value_on_iter_n_itself_still_expands() {
        use serde_json::json;
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "each_in_path": "prime.items.value", "progress": {"done": 2, "total": 2}},
                "iter_0": {
                    "nap": {"value": "x\n", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z", "provider": "shell"}}
                },
                "iter_1": {
                    "nap": {"value": "x\n", "meta": {"created_at": "2026-01-01T00:00:01Z", "completed_at": "2026-01-01T00:00:02Z", "provider": "shell"}}
                }
            }
        }});
        let flat = crate::model::flatten_state(&state);
        let model = RunModel::new();
        let plan = PlanTree::empty();
        let header = header_with(Some("2026-01-01T00:00:00Z"), None);
        let rows = vec![loop_row("prime.fan")];
        let time_rows = build_time_rows(&rows, &flat, &model, &plan, &header, &BTreeMap::new());
        assert!(
            time_rows[0].loop_expandable,
            "a chain loop with no value on iter_n itself is still expandable"
        );

        let passes = expand_loop(
            "prime.fan",
            &flat,
            &model,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:02Z",
        );
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].label, "pass 1");
        assert_eq!(passes[0].start_s, Some(0.0));
        assert_eq!(passes[0].duration_s, Some(1.0));
        assert_eq!(passes[1].label, "pass 2");
        assert_eq!(passes[1].start_s, Some(1.0));
        assert_eq!(passes[1].duration_s, Some(1.0));
    }

    #[test]
    fn a_named_tree_loops_overlapping_passes_both_show_their_own_real_span() {
        use serde_json::json;
        // A tree loop's two passes run at once: pass 0 is 0-20s,
        // pass 1 is 10-30s -- the overlap is shown, not merged away,
        // since each pass is its own row once expanded.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "items": {"value": null, "meta": {"completed_at": null},
                "iter_0": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:20Z"}},
                "iter_1": {"value": null, "meta": {"created_at": "2026-01-01T00:00:10Z", "completed_at": "2026-01-01T00:00:30Z"}}
            }
        }});
        let flat = crate::model::flatten_state(&state);
        let model = RunModel::new();
        let passes = expand_loop(
            "prime.items",
            &flat,
            &model,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:30Z",
        );
        assert_eq!(passes.len(), 2);
        let (p0_start, p0_dur) = (passes[0].start_s.unwrap(), passes[0].duration_s.unwrap());
        let (p1_start, p1_dur) = (passes[1].start_s.unwrap(), passes[1].duration_s.unwrap());
        assert_eq!((p0_start, p0_start + p0_dur), (0.0, 20.0));
        assert_eq!((p1_start, p1_start + p1_dur), (10.0, 30.0));
    }

    /// An unnamed loop's own body never gets a `RowKind::Loop` row
    /// of its own (DESIGN.md §1.3) -- it shares its leaf's row and
    /// path, so a real compiled plan's `PlanEntryKind::Loop` for it
    /// sits at the leaf's own *parent* path, not the leaf's.
    fn unnamed_loop_plan() -> PlanTree {
        use electricity_bytecode::{
            EffectPath, LeafKind, LoopFlow, LoopSpec, NodeKind, OnError, Op, ParamNode, Program,
            Region, ToolOp,
        };
        let root_path = EffectPath::root();
        let loop_path = root_path.clone();
        let body_path = loop_path.clone().push_name("u_nap");
        let tool = Op {
            path: body_path,
            name: Some("u_nap".to_string()),
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
        };
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: loop_path,
                    name: None,
                    kind: NodeKind::Control(Region::Loop {
                        name: None,
                        spec: LoopSpec::Each {
                            in_path: "prime.items".to_string(),
                            as_name: "item".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![tool],
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

    fn tool_row(path: &str) -> Row {
        Row {
            path: path.to_string(),
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
        }
    }

    #[test]
    fn an_unnamed_loops_body_leaf_expands_by_event_id_when_events_are_flowing() {
        let plan = unnamed_loop_plan();
        let flat = BTreeMap::new();
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
        let header = header_with(Some("2026-01-01T00:00:00Z"), None);
        let rows = vec![tool_row("prime.u_nap")];
        let time_rows = build_time_rows(&rows, &flat, &model, &plan, &header, &BTreeMap::new());
        assert_eq!(
            time_rows[0].kind,
            RowKind::Tool,
            "an unnamed loop's body never gets its own RowKind::Loop row"
        );
        assert!(time_rows[0].loop_expandable);

        let passes = expand_loop(
            "prime.u_nap",
            &flat,
            &model,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:12Z",
        );
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].label, "#1");
        assert_eq!(passes[1].label, "#2");
    }

    #[test]
    fn an_unnamed_loops_body_leaf_with_no_events_stays_one_row() {
        let plan = unnamed_loop_plan();
        let flat = BTreeMap::new();
        let model = RunModel::new();
        let header = header_with(Some("2026-01-01T00:00:00Z"), None);
        let rows = vec![tool_row("prime.u_nap")];
        let time_rows = build_time_rows(&rows, &flat, &model, &plan, &header, &BTreeMap::new());
        assert!(!time_rows[0].loop_expandable);
    }
}
