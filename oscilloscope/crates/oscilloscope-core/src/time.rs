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
use crate::plan::PlanTree;
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
/// now line never sits flush against the pane's own right edge, but
/// proportional to the span itself so a young run's bars aren't
/// squeezed into a narrow strip on the left -- a fixed 30s floor
/// would put a 10s-old run's now line a quarter of the way across
/// the axis.
fn axis_margin_s(elapsed_s: f64) -> f64 {
    (elapsed_s * 0.1).max(2.0)
}

/// The timeline's own clock axis (DESIGN.md's "Clock axis"): start to
/// the later of `now` and the ETA's p90 end (plus a margin) while the
/// run goes, or start to end once it has one. Below the p10/p90 run
/// threshold, the ETA itself (`history_eta_ts`, gated at a lower run
/// count) stands in for its own missing p90 -- every pending step
/// with history still gets a ghost bar from the run's first frame,
/// rather than only the next one, whose own axis the running row's
/// bar alone already reached.
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
        .or_else(|| header.history_eta_ts.clone())
        .or_else(|| header.innermost_loop_eta_ts.clone());
    let later_parsed = p90_candidate
        .as_deref()
        .and_then(crate::diff::parse_instant);
    let (later_ts, gap_past_now_s) = match (later_parsed, now_dt) {
        (Some(p90), Some(now_parsed)) if p90 > now_parsed => (
            p90_candidate.unwrap(),
            (p90 - now_parsed).num_milliseconds() as f64 / 1000.0,
        ),
        _ => (now.to_string(), 0.0),
    };
    // The margin's own job is keeping the now line off the pane's
    // right edge at all -- when the ETA already reaches a decent way
    // past now on its own, topping up a margin on top of *that* on
    // top of the usual proportional one would double-count the same
    // gap, crowding a run with an active loop ETA back toward the
    // left: a chain loop a couple of seconds into its own first pass,
    // little elapsed but already 3s from its own ETA, would then read
    // with now nowhere near a comfortable majority of the way across
    // the pane. Only the shortfall below the usual margin is added.
    // `.max(1e-3)`, not `.max(0.0)`: the axis's own end must always
    // sit strictly past `later_ts`, even when the ETA already leaves
    // ample room on its own, since a margin of literally zero would
    // draw the ETA's own marker flush against the pane's right edge.
    let margin_s = (axis_margin_s(header.elapsed_s) - gap_past_now_s).max(1e-3);
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
    // its own, so its leaf shares the loop's own row. Checking the
    // leaf's own path against `is_unnamed_loop_body_path` (not its
    // parent's entries) is what keeps a sibling such as `prime.a`,
    // which sits next to the loop at the same parent path but isn't
    // part of its body, from expanding too.
    if !plan.is_unnamed_loop_body_path(path) {
        return false;
    }
    // Without `--events`, successive passes reuse the same path with
    // no id to tell them apart (DESIGN.md: "they can't be told apart,
    // and the loop stays one row"); a single pass so far has nothing
    // to expand into either.
    model.has_events()
        && model
            .step_intervals()
            .iter()
            .filter(|i| i.path == path)
            .count()
            > 1
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
            let loop_expandable = loop_is_expandable(&row.path, flat, model, plan, row.kind);
            let state_start_s = row_start_s(&row.path, flat, model, run_start_ts);
            // An unnamed loop's body shares its row with the leaf
            // underneath it, which has no span or progress of its
            // own to read from state alone at all -- state keeps only
            // the *last* pass's own node, overwritten by each pass in
            // turn. Collapsed, its row reads the whole loop's own
            // span (DESIGN.md) instead: its first pass's own start,
            // from the same event id pairs `expand_loop` reads for
            // its own rows (the one thing events alone can answer),
            // combined with state's own last-pass end (already
            // extended to now while it's still running, the same way
            // every other row's own `duration_s` is) -- never an
            // event-sourced end of its own, which a `use` step right
            // after it could, by a few milliseconds of clock skew
            // between the two logs, start running before.
            let unnamed_loop_body = model.has_events() && plan.is_unnamed_loop_body_path(&row.path);
            let first_pass = unnamed_loop_body
                .then(|| unnamed_loop_first_pass_start(&row.path, model, run_start_ts))
                .flatten();
            let (start_s, duration_s, loop_progress) = match first_pass {
                Some((first_s, count)) => {
                    let end_s = state_start_s.zip(row.duration_s).map(|(s, d)| s + d);
                    let start_s = Some(
                        end_s.map_or(first_s, |_| first_s.min(state_start_s.unwrap_or(first_s))),
                    );
                    (
                        start_s,
                        end_s.zip(start_s).map(|(e, s)| e - s),
                        Some(LoopProgress {
                            done: count as u64,
                            // The total is never known for an unnamed
                            // loop while it's still running -- shown
                            // as `?` until the last pass finishes,
                            // with the one in progress not counted
                            // among `done`.
                            total: (row.status.kind != StatusKind::Running).then_some(count as u64),
                            eta_s: None,
                        }),
                    )
                }
                None => (state_start_s, row.duration_s, row.loop_progress),
            };
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
                duration_s,
                loop_progress,
                has_children: row.has_children,
                loop_expandable,
                ghost,
            }
        })
        .collect()
}

/// An unnamed loop's own body row, collapsed (DESIGN.md): its first
/// pass's own start, run-relative to `run_start_ts` -- the one thing
/// state alone can never answer, since each later pass overwrites the
/// same path's node -- and how many passes have finished. `None` with
/// no finished pass logged at this path at all yet (the row still
/// reads from state alone, same as before this existed).
pub(crate) fn unnamed_loop_first_pass_start(
    path: &str,
    model: &RunModel,
    run_start_ts: &str,
) -> Option<(f64, usize)> {
    let mut start_s: Option<f64> = None;
    let mut count = 0usize;
    let offsets = model.step_interval_offsets(run_start_ts);
    for (interval, &(s, _)) in model.step_intervals().iter().zip(offsets.iter()) {
        if interval.path != path {
            continue;
        }
        count += 1;
        if !s.is_nan() {
            start_s = Some(start_s.map_or(s, |cur: f64| cur.min(s)));
        }
    }
    Some((start_s?, count))
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
            // A `BTreeMap`'s own sorted order makes every key that is
            // either `pass_path` itself or starts with `pass_prefix`
            // contiguous, starting right at `pass_path` -- any byte
            // after the shared `"iter_{index}"` prefix that isn't
            // `.` (a later pass's own digits, `iter_30` after
            // `iter_3`, or an unrelated sibling) sorts strictly past
            // every `pass_prefix`-matching key, so the scan can break
            // the moment it sees one, rather than walking every
            // entry in `flat` once per pass (O(passes × flat)
            // otherwise).
            for (child_path, node) in flat.range(pass_path.clone()..) {
                if child_path != &pass_path && !child_path.starts_with(&pass_prefix) {
                    break;
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
    let mut rows: Vec<LoopPassRow> = model
        .step_intervals()
        .iter()
        .filter(|i| i.path == path)
        .enumerate()
        .map(|(n, interval)| {
            let start_s = duration_seconds(run_start_ts, &interval.start_ts);
            let duration_s = duration_seconds(&interval.start_ts, &interval.end_ts);
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
        .collect();
    // A pass still running when expanded lists as its own row too
    // (DESIGN.md), numbered right after every finished pass -- it has
    // no `end` event yet, so it never reaches the loop above.
    let finished = rows.len();
    for (n, start_ts) in model.open_starts_at(path).into_iter().enumerate() {
        let start_s = duration_seconds(run_start_ts, start_ts);
        let duration_s = start_s.and_then(|s| duration_seconds(run_start_ts, now).map(|e| e - s));
        rows.push(LoopPassRow {
            label: format!("#{}", finished + n + 1),
            status: RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            start_s,
            duration_s,
        });
    }
    rows
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
    /// `(state)`").
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
    // `step_interval_offsets` parses each interval's own timestamps
    // once and caches the result -- the log only grows, so a later
    // frame parses just its own new tail, not the whole thing again.
    for &(s, e) in model.step_interval_offsets(run_start_ts).iter() {
        if !s.is_nan() {
            points.push((s, 1));
        }
        if !e.is_nan() {
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

/// "events / min ... quiet = agent" (DESIGN.md) -- shown when
/// exactly one leaf is running right now and it's an agent (a
/// `RowKind::Tool` with provider `agent`, `screens::is_agent_row` --
/// never only a `RowKind::Prompt`, which is an ordinary prompt, not
/// necessarily an agent at all), and the strip's own rate has
/// actually dropped (`recent_rate`, the caller's own count of
/// start/end events in the last 60s, not counting the agent's own
/// start event, DESIGN.md), not merely because an agent happens to
/// be running.
pub fn quiet_agent(running_leaf_is_agent: &[bool], recent_rate: u64) -> bool {
    running_leaf_is_agent == [true] && recent_rate == 0
}

/// The next round local clock time at or after `from_utc`, a whole
/// multiple of `tick_s` seconds since local midnight in `zone` --
/// this function's own caller (`axis_tick_columns`'s `ticks_at`)
/// calls it again for every tick, from just past the last one,
/// rather than stepping forward by a fixed `tick_s`-sized duration in
/// absolute time: measuring from *local* midnight every time, not a
/// stored UTC instant, is what keeps an hour tick on the same local
/// whole hour even across a daylight-saving change, and what puts one
/// on a local whole hour at all in a zone offset by a half hour from
/// UTC.
fn next_round_local_time(
    from_utc: chrono::DateTime<chrono::Utc>,
    tick_s: f64,
    zone: &dyn crate::format::ClockZone,
) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    let local = zone.local(from_utc);
    let offset = *local.offset();
    let midnight = local.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let elapsed_s = (local.naive_local() - midnight).num_milliseconds() as f64 / 1000.0;
    let next_elapsed_s = (elapsed_s / tick_s).ceil() * tick_s;
    let next_local_naive =
        midnight + chrono::Duration::milliseconds((next_elapsed_s * 1000.0).round() as i64);
    let next_local = offset
        .from_local_datetime(&next_local_naive)
        .single()
        .unwrap_or_else(|| offset.from_utc_datetime(&next_local_naive));
    next_local.with_timezone(&chrono::Utc)
}

/// The clock axis's own tick columns (DESIGN.md: "labelled ticks at
/// round clock times ... whichever keeps labels about 10 columns
/// apart") -- one entry per tick: its own column, its run-relative
/// offset, and its label, `HH:MM:SS` for a sub-minute tick interval
/// (so a run only tens of seconds old still gets several labels) or
/// `HH:MM` otherwise. Ticks fall on round clock times in `zone`,
/// never on `axis_start_s` merely because that's where the axis
/// happens to begin.
pub fn axis_tick_columns(
    run_start_ts: &str,
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    zone: &dyn crate::format::ClockZone,
) -> Vec<(usize, f64, String)> {
    let width = width.max(1);
    let span = (axis_end_s - axis_start_s).max(0.0);
    if span <= 0.0 {
        return Vec::new();
    }
    let Some(run_start) = crate::diff::parse_instant(run_start_ts) else {
        return Vec::new();
    };
    let ticks_at = |tick_s: f64, fmt: &str| -> Vec<(usize, f64, String)> {
        if tick_s <= 0.0 {
            return Vec::new();
        }
        let axis_start_utc = (run_start
            + chrono::Duration::milliseconds((axis_start_s * 1000.0).round() as i64))
        .with_timezone(&chrono::Utc);
        let axis_end_utc = (run_start
            + chrono::Duration::milliseconds((axis_end_s * 1000.0).round() as i64))
        .with_timezone(&chrono::Utc);
        // Crosses a local day when the axis's own start and end fall
        // on different local calendar dates -- a multi-day span's own
        // local-midnight ticks then read the date instead of `00:00`,
        // which would otherwise repeat with nothing to tell the days
        // apart.
        let crosses_a_day =
            zone.local(axis_start_utc).date_naive() != zone.local(axis_end_utc).date_naive();
        let mut out = Vec::new();
        let mut tick = next_round_local_time(axis_start_utc, tick_s, zone);
        while tick <= axis_end_utc + chrono::Duration::milliseconds(1) {
            let offset_s =
                (tick - run_start.with_timezone(&chrono::Utc)).num_milliseconds() as f64 / 1000.0;
            if let Some(col) = now_column(axis_start_s, axis_end_s, width, offset_s) {
                let local = zone.local(tick);
                let label = if crosses_a_day && local.format("%H:%M:%S").to_string() == "00:00:00" {
                    local.format("%m-%d").to_string()
                } else {
                    local.format(fmt).to_string()
                };
                out.push((col, offset_s, label));
            }
            // Recomputed from this tick's own local midnight every
            // time, not stepped forward by a fixed absolute duration:
            // a daylight-saving change (or, for a day-or-longer tick,
            // any calendar day at all) can make the real duration to
            // the next round local time something other than
            // `tick_s` itself.
            tick = next_round_local_time(tick + chrono::Duration::milliseconds(1), tick_s, zone);
        }
        out
    };
    let tick_s = crate::format::clock_tick_seconds(span, width, 10);
    let fmt = if tick_s < 60.0 { "%H:%M:%S" } else { "%H:%M" };
    ticks_at(tick_s, fmt)
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
/// running at once") -- per column, the *most* intervals overlapping
/// at any single instant within it, not how many merely touch it: a
/// sweep over every interval's own start/end (ends processed before
/// starts at an equal time, so a purely sequential run never double-
/// counts the instant one step ends and the next begins), carrying
/// each resulting overlap level into every column its own time range
/// spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum InFlightEdge {
    End,
    Start,
}

pub fn in_flight_heights(
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    intervals: &[(f64, f64)],
) -> Vec<u64> {
    let width = width.max(1);
    let mut heights = vec![0u64; width];
    if axis_end_s <= axis_start_s {
        return heights;
    }
    let span = axis_end_s - axis_start_s;
    // Rounded to the millisecond: an interval's own two ends can come
    // from state (microsecond precision) while a neighbour's comes
    // from `--events` (millisecond precision, DESIGN.md §3's format
    // table) -- without this, clock skew between the two logs can
    // round-trip as a few *microseconds* of apparent overlap between
    // two steps that never actually ran at the same time at all, one
    // sequential run briefly reading 2 in flight instead of 1.
    let round_ms = |t: f64| (t * 1000.0).round() / 1000.0;
    let mut events: Vec<(f64, InFlightEdge)> = Vec::with_capacity(intervals.len() * 2);
    for &(start, end) in intervals {
        // A non-finite edge (never produced by a real input) sorts
        // next to itself under `total_cmp` but compares false to
        // every other value under `==`, which would stall the sweep
        // below forever looking for a match that's already past --
        // skipped here instead of ever reaching it.
        if !start.is_finite() || !end.is_finite() {
            continue;
        }
        let start = round_ms(start.clamp(axis_start_s, axis_end_s));
        let end = round_ms(end.clamp(axis_start_s, axis_end_s));
        if end <= start {
            continue;
        }
        events.push((start, InFlightEdge::Start));
        events.push((end, InFlightEdge::End));
    }
    if events.is_empty() {
        return heights;
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    // The column an instant `t` opens (its own time falls at or after
    // the column's own start).
    let col_start_of = |t: f64| {
        (((t - axis_start_s) / span) * width as f64)
            .floor()
            .clamp(0.0, (width - 1) as f64) as usize
    };
    // The last column a half-open span ending at `t` (exclusive)
    // still reaches -- `ceil() - 1` rather than `floor()`, so a span
    // ending exactly on a column boundary doesn't bleed one column
    // too far into the next.
    let col_end_of = |t: f64| {
        ((((t - axis_start_s) / span) * width as f64).ceil() - 1.0).clamp(0.0, (width - 1) as f64)
            as usize
    };
    let mut current: i64 = 0;
    let mut t = axis_start_s;
    let mut idx = 0usize;
    while idx < events.len() {
        let next_t = events[idx].0;
        if next_t > t && current > 0 {
            let col_from = col_start_of(t);
            let col_to = col_end_of(next_t).max(col_from);
            for height in &mut heights[col_from..=col_to] {
                *height = (*height).max(current as u64);
            }
        }
        while idx < events.len() && events[idx].0 == next_t {
            match events[idx].1 {
                InFlightEdge::Start => current += 1,
                InFlightEdge::End => current -= 1,
            }
            idx += 1;
        }
        t = next_t;
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
/// finished"). `running` is the call currently in progress, if any
/// ([`running_tool_elapsed_ms`]) -- its own elapsed time is folded
/// into that tool's total before picking the busiest one, since
/// `tools.<name>.ms` only ever counts calls that have already
/// finished, which left the footer reading a stale `0.0s` for
/// seconds while the first call of a run was still going.
pub fn zoom_agent_footer(
    turns: Option<u64>,
    tool_calls: Option<u64>,
    tools: &BTreeMap<String, crate::model::AgentLiveToolUsage>,
    elapsed_s: f64,
    running: Option<(String, u64)>,
) -> ZoomAgentFooter {
    let mut tools = tools.clone();
    if let Some((name, ms)) = running {
        tools.entry(name).or_default().ms += ms;
    }
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

/// The tool call currently in progress, for [`zoom_agent_footer`]'s
/// own running-elapsed fix: the transcript's own unmatched `tool_
/// start` when there is one, else `current_tool` and its own `started
/// _at` when there's no transcript at all. `None` while nothing is
/// actually running, or when the running call's own start can't be
/// placed on the run's clock.
pub fn running_tool_elapsed_ms(
    calls: &[ToolCall],
    current_tool: Option<(&str, &str)>,
    now_s: f64,
    run_start_ts: &str,
) -> Option<(String, u64)> {
    if let Some(call) = calls.iter().rev().find(|c| c.is_running()) {
        let start_s = duration_seconds(run_start_ts, call.start_t.as_deref()?)?;
        let ms = ((now_s - start_s) * 1000.0).max(0.0) as u64;
        return Some((call.tool.clone(), ms));
    }
    let (name, started_at) = current_tool?;
    let start_s = duration_seconds(run_start_ts, started_at)?;
    let ms = ((now_s - start_s) * 1000.0).max(0.0) as u64;
    Some((name.to_string(), ms))
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
    if stats.count < 3 {
        return None;
    }
    // A difference under a second is noise, not a real signal, no
    // matter what the percentage rules below would say about it (a
    // step taking a few milliseconds reads `0.0s` either way, so the
    // 20%/p10-p90 rules see nothing but rounding).
    if (now_s - p50).abs() < 1.0 {
        return Some(VsUsualMark::Usual);
    }
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
    let delta = (now_s - p50) / p50.max(f64::MIN_POSITIVE);
    Some(if delta > 0.2 {
        VsUsualMark::Slower
    } else if !running && delta < -0.2 {
        VsUsualMark::Faster
    } else {
        VsUsualMark::Usual
    })
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

    /// 10s into a run with no history or loop ETA at all, `now` must
    /// sit well past the axis's own midpoint -- a fixed 30s floor
    /// (`max(5% of elapsed, 30s)`) would put it at a quarter of the
    /// way across instead.
    #[test]
    fn axis_margin_is_proportional_not_a_fixed_30s_floor() {
        let mut header = header_with(Some("2026-01-01T00:00:00Z"), None);
        header.elapsed_s = 10.0;
        let now = "2026-01-01T00:00:10Z";
        let span = axis_span(&header, now);
        let start_dt = crate::diff::parse_instant(span.start_ts.as_deref().unwrap()).unwrap();
        let end_dt = crate::diff::parse_instant(&span.end_ts).unwrap();
        let now_dt = crate::diff::parse_instant(now).unwrap();
        let total_s = (end_dt - start_dt).num_milliseconds() as f64 / 1000.0;
        let now_s = (now_dt - start_dt).num_milliseconds() as f64 / 1000.0;
        assert!(
            now_s / total_s >= 0.6,
            "now sits at {:.0}% of the axis",
            now_s / total_s * 100.0
        );
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

    /// An ETA already behind `now` (a run overrunning its own
    /// history) must not pull the axis back to end *before* `now` --
    /// it falls back to the plain `now`-plus-margin end, the same as
    /// with no ETA at all.
    #[test]
    fn axis_span_ignores_an_eta_already_in_the_past() {
        let mut header = header_with(Some("2026-01-01T00:00:00Z"), None);
        header.history_eta_high_ts = Some("2026-01-01T00:04:00Z".to_string());
        let now = "2026-01-01T00:05:00Z";
        let span = axis_span(&header, now);
        assert!(span.show_now);
        let now_dt = crate::diff::parse_instant(now).unwrap();
        let end_dt = crate::diff::parse_instant(&span.end_ts).unwrap();
        assert!(
            end_dt > now_dt,
            "the axis must still reach past now, not stop at the stale ETA: {span:?}"
        );
    }

    /// Below the p10/p90 run threshold (3 or 4 ok runs), there's no
    /// `history_eta_high_ts` at all -- the axis still reaches the
    /// plain ETA (`history_eta_ts`), not just past `now`, so every
    /// pending step with history gets a ghost bar from the first
    /// frame.
    #[test]
    fn axis_span_reaches_the_plain_eta_when_there_is_no_p90() {
        let mut header = header_with(Some("2026-01-01T00:00:00Z"), None);
        header.history_eta_ts = Some("2026-01-01T00:30:00Z".to_string());
        let span = axis_span(&header, "2026-01-01T00:05:00Z");
        let eta_dt = crate::diff::parse_instant("2026-01-01T00:30:00Z").unwrap();
        let end_dt = crate::diff::parse_instant(&span.end_ts).unwrap();
        assert!(end_dt > eta_dt);
    }

    /// An active loop ETA already a few seconds
    /// ahead of now must not get an *additional* full margin stacked
    /// on top of it -- only enough to cover any shortfall below the
    /// usual one, so a run barely any older than its own innermost
    /// loop's ETA still reads with `now` comfortably past the
    /// midpoint, not squeezed left the same way a too-wide margin
    /// once squeezed every run.
    #[test]
    fn axis_span_margin_tops_up_rather_than_stacks_onto_an_already_wide_eta_gap() {
        let mut header = header_with(Some("2026-01-01T00:00:00Z"), None);
        header.elapsed_s = 6.5;
        header.innermost_loop_eta_ts = Some("2026-01-01T00:00:09.5Z".to_string());
        let now = "2026-01-01T00:00:06.5Z";
        let span = axis_span(&header, now);
        let start_dt = crate::diff::parse_instant(span.start_ts.as_deref().unwrap()).unwrap();
        let end_dt = crate::diff::parse_instant(&span.end_ts).unwrap();
        let now_dt = crate::diff::parse_instant(now).unwrap();
        let total_s = (end_dt - start_dt).num_milliseconds() as f64 / 1000.0;
        let now_s = (now_dt - start_dt).num_milliseconds() as f64 / 1000.0;
        assert!(end_dt > crate::diff::parse_instant("2026-01-01T00:00:09.5Z").unwrap());
        assert!(
            now_s / total_s >= 0.6,
            "now sits at {:.0}% of the axis, squeezed left",
            now_s / total_s * 100.0
        );
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
    fn quiet_agent_is_true_only_for_a_lone_quiet_running_agent() {
        assert!(quiet_agent(&[true], 0));
        assert!(!quiet_agent(&[true], 2), "the rate must be low too");
        assert!(!quiet_agent(&[false], 0), "a non-agent leaf isn't an agent");
        assert!(!quiet_agent(&[true, false], 0));
        assert!(!quiet_agent(&[], 0));
    }

    #[test]
    fn axis_tick_columns_keeps_ticks_about_10_columns_apart() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T00:00:00Z", 0.0, 600.0, 60, &utc);
        assert!(ticks.len() >= 2);
        for window in ticks.windows(2) {
            assert!(window[1].0 - window[0].0 >= 9);
        }
        assert_eq!(ticks[0].1 % 60.0, 0.0);
    }

    #[test]
    fn axis_tick_columns_is_empty_for_a_zero_span() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        assert_eq!(
            axis_tick_columns("2026-01-01T00:00:10Z", 10.0, 10.0, 40, &utc),
            Vec::new()
        );
    }

    /// A run starting at a non-round time (11:46:43) must still tick
    /// at round minutes, never at the run's own start.
    #[test]
    fn axis_tick_columns_ticks_at_round_minutes_not_the_runs_own_start() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T11:46:43Z", 0.0, 300.0, 60, &utc);
        assert!(!ticks.is_empty());
        assert_ne!(
            ticks[0].1, 0.0,
            "the first tick must not sit at the run's own start"
        );
        // The run starts at :43 seconds past its own minute -- a
        // round-minute tick's own absolute second-of-minute is 0.
        for (_, offset_s, label) in &ticks {
            assert_eq!(label.chars().count(), 5, "{label} should read HH:MM");
            assert_eq!(
                (43.0 + offset_s) % 60.0,
                0.0,
                "{label} is not a round minute"
            );
        }
    }

    /// A +05:30 zone still ticks on a local whole hour, not a UTC
    /// one.
    #[test]
    fn axis_tick_columns_ticks_on_local_whole_hours_in_a_half_hour_zone() {
        let ist = chrono::FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        // 00:00:00Z is 05:30:00 IST -- the run spans several hours,
        // so the tick interval lands on whole IST hours.
        let ticks = axis_tick_columns("2026-01-01T00:00:00Z", 0.0, 6.0 * 3600.0, 80, &ist);
        assert!(!ticks.is_empty());
        for (_, _, label) in &ticks {
            assert!(label.ends_with(":00"), "{label} is not a round IST hour");
        }
    }

    /// A short, 20s-old run that hasn't crossed a minute boundary has
    /// no round minute of its own to tick on -- it still gets
    /// sub-minute labels (`HH:MM:SS`), not nothing at all.
    #[test]
    fn axis_tick_columns_labels_sub_minute_intervals_with_seconds() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T00:00:10Z", 0.0, 20.0, 40, &utc);
        assert!(!ticks.is_empty());
        for (_, offset_s, label) in &ticks {
            assert_eq!(label.chars().count(), 8, "{label} should read HH:MM:SS");
            assert_eq!((10.0 + offset_s) % 5.0, 0.0);
        }
    }

    /// Spacing alone picks the tick interval -- a round minute inside
    /// a short span doesn't win over a tighter sub-minute tick that
    /// also keeps labels far enough apart.
    #[test]
    fn axis_tick_columns_picks_sub_minute_ticks_by_spacing_even_when_a_round_minute_exists() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T00:00:45Z", 0.0, 20.0, 96, &utc);
        assert!(
            ticks.len() > 1,
            "expected several sub-minute ticks, got {ticks:?}"
        );
        for (_, _, label) in &ticks {
            assert_eq!(label.chars().count(), 8, "{label} should read HH:MM:SS");
        }
    }

    /// A finished 20s run that crosses a minute boundary ticks
    /// several times, not once -- the same candidate list a longer
    /// run uses, picked by spacing alone.
    #[test]
    fn axis_tick_columns_a_20s_span_crossing_a_minute_gets_several_labels() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T00:00:45Z", 0.0, 20.0, 70, &utc);
        assert!(
            ticks.len() > 1,
            "expected several ticks across the minute boundary, got {ticks:?}"
        );
        for (_, _, label) in &ticks {
            assert_eq!(label.chars().count(), 8, "{label} should read HH:MM:SS");
        }
        assert!(
            ticks.iter().any(|(_, _, label)| label.starts_with("00:01")),
            "expected at least one tick past the minute boundary, got {ticks:?}"
        );
    }

    /// A long, 6h span still only ticks on round hours.
    #[test]
    fn axis_tick_columns_over_a_six_hour_span_ticks_on_round_hours() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T00:00:00Z", 0.0, 6.0 * 3600.0, 80, &utc);
        assert!(!ticks.is_empty());
        for (_, offset_s, _) in &ticks {
            assert_eq!(offset_s % 3600.0, 0.0);
        }
    }

    /// A `ClockZone` modelling the one hour US "spring forward"
    /// change at 2026-03-08 07:00 UTC (02:00 EST jumps straight to
    /// 03:00 EDT) -- enough to exercise a real daylight-saving
    /// transition without a timezone database dependency.
    struct SpringForwardZone;

    impl crate::format::ClockZone for SpringForwardZone {
        fn local(
            &self,
            utc: chrono::DateTime<chrono::Utc>,
        ) -> chrono::DateTime<chrono::FixedOffset> {
            let transition = chrono::DateTime::parse_from_rfc3339("2026-03-08T07:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
            let offset = if utc < transition {
                chrono::FixedOffset::west_opt(5 * 3600).unwrap()
            } else {
                chrono::FixedOffset::west_opt(4 * 3600).unwrap()
            };
            utc.with_timezone(&offset)
        }
    }

    /// Hour ticks must stay on the same local clock time across a
    /// daylight-saving change -- recomputed from each tick's own
    /// local midnight, not stepped forward by a fixed absolute
    /// duration, which would otherwise drift by the one skipped hour
    /// from the transition on.
    #[test]
    fn axis_tick_columns_a_us_spring_forward_span_skips_the_missing_hour() {
        let zone = SpringForwardZone;
        // Run starts at 2026-03-08T00:00 EST (05:00 UTC) and spans 6
        // real hours, to 2026-03-08T11:00 UTC (06:00 EDT local, since
        // one hour vanished at 02:00 EST).
        let ticks = axis_tick_columns("2026-03-08T05:00:00Z", 0.0, 6.0 * 3600.0, 80, &zone);
        let labels: Vec<&str> = ticks.iter().map(|(_, _, l)| l.as_str()).collect();
        assert!(labels.contains(&"01:00"), "{labels:?}");
        assert!(
            !labels.contains(&"02:00"),
            "02:00 never exists this day: {labels:?}"
        );
        assert!(labels.contains(&"03:00"), "{labels:?}");
        let at = |label: &str| ticks.iter().find(|(_, _, l)| l == label).unwrap().1;
        assert_eq!(
            at("03:00") - at("01:00"),
            3600.0,
            "only one real hour passes between them, not two: {ticks:?}"
        );
    }

    /// A span crossing a local day labels the tick at local midnight
    /// with the date, since `00:00` alone would repeat with nothing
    /// to tell the days apart.
    #[test]
    fn axis_tick_columns_over_a_30_hour_span_dates_the_midnight_tick() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T12:00:00Z", 0.0, 30.0 * 3600.0, 100, &utc);
        let labels: Vec<&str> = ticks.iter().map(|(_, _, l)| l.as_str()).collect();
        assert!(
            labels.contains(&"01-02"),
            "the midnight tick must read the date: {labels:?}"
        );
        assert!(
            !labels.contains(&"00:00"),
            "never a bare, ambiguous 00:00 once the span crosses a day: {labels:?}"
        );
    }

    /// An 8-second run gets exactly one round tick, not zero and not
    /// several crowded together.
    #[test]
    fn axis_tick_columns_an_8_second_span_gets_a_single_label() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let ticks = axis_tick_columns("2026-01-01T00:00:06Z", 0.0, 8.0, 20, &utc);
        assert_eq!(ticks.len(), 1, "{ticks:?}");
        assert_eq!(ticks[0].2, "00:00:10");
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

    /// A purely sequential run (no step overlaps another) must read
    /// 1 everywhere it runs at all, never 2 at the instant one step
    /// ends and the next begins.
    #[test]
    fn in_flight_heights_back_to_back_intervals_never_double_count() {
        let intervals = vec![(0.0, 20.0), (20.0, 40.0), (40.0, 60.0)];
        let heights = in_flight_heights(0.0, 60.0, 6, &intervals);
        assert_eq!(heights, vec![1, 1, 1, 1, 1, 1]);
    }

    /// One interval's own end came from state (microsecond
    /// precision), the next's own start from `--events` (millisecond
    /// precision) -- a few dozen microseconds apart from rounding
    /// alone, never a real overlap, which this must still read as a
    /// purely sequential run (max 1), not 2.
    #[test]
    fn in_flight_heights_tolerates_sub_millisecond_clock_skew_between_sources() {
        let intervals = vec![(0.0, 3.589970), (3.590000, 10.0)];
        let heights = in_flight_heights(0.0, 10.0, 10, &intervals);
        assert_eq!(heights.iter().copied().max(), Some(1), "{heights:?}");
    }

    /// A non-finite edge (never produced by a real input) must be
    /// skipped, not left to stall the sweep: it sorts into the event
    /// list under `total_cmp` but never compares equal to anything
    /// under `==`, so the loop that walks events sharing one instant
    /// would spin forever looking for a match if this weren't
    /// filtered out first.
    #[test]
    fn in_flight_heights_skips_a_non_finite_edge_without_hanging() {
        let intervals = vec![(0.0, 10.0), (f64::NAN, 5.0), (3.0, f64::NAN), (2.0, 8.0)];
        let heights = in_flight_heights(0.0, 10.0, 10, &intervals);
        assert_eq!(heights.iter().copied().max(), Some(2), "{heights:?}");
    }

    /// Three intervals overlap at once, and a fourth starts exactly
    /// where they end -- the true max is 3, not 4 (a touch-counting
    /// rule's answer instead).
    #[test]
    fn in_flight_heights_true_max_overlap_is_3_not_4() {
        let intervals = vec![(0.0, 30.0), (0.0, 30.0), (0.0, 30.0), (30.0, 60.0)];
        let heights = in_flight_heights(0.0, 60.0, 6, &intervals);
        assert_eq!(heights.iter().copied().max(), Some(3));
    }

    /// Zoomed into a partial window in the middle of a run (`+`):
    /// an interval that runs the whole way through it, well past
    /// both edges, must still read as in flight across the entire
    /// visible window -- clamped to it, not dropped for reaching
    /// outside it -- while one that never reaches the window at all
    /// contributes nothing.
    #[test]
    fn in_flight_heights_clamps_an_interval_to_a_partial_window() {
        let spans_the_whole_window = vec![(0.0, 100.0)];
        let heights = in_flight_heights(40.0, 60.0, 4, &spans_the_whole_window);
        assert_eq!(heights, vec![1, 1, 1, 1]);

        let never_reaches_the_window = vec![(0.0, 10.0)];
        let heights = in_flight_heights(40.0, 60.0, 4, &never_reaches_the_window);
        assert_eq!(heights, vec![0, 0, 0, 0]);
    }

    /// [`in_flight_heights_clamps_an_interval_to_a_partial_window`],
    /// for the events/min and decisions strips' own shared bucketing
    /// -- a point outside a zoomed-in partial window must be dropped
    /// entirely, not folded into the nearest edge column.
    #[test]
    fn bucket_weighted_drops_points_outside_a_partial_window() {
        let points = vec![(10.0, 1), (50.0, 1), (90.0, 1)];
        let heights = bucket_weighted(40.0, 60.0, 4, &points);
        assert_eq!(
            heights.iter().sum::<u64>(),
            1,
            "only the 50.0 point is inside"
        );
        assert_eq!(heights[2], 1, "50.0 is 50% across 40..60, column 2 of 4");
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

    /// A quick step's own noise (a few milliseconds either way)
    /// must never read `▲`/`▼` -- a difference under a second is
    /// always `≈`.
    #[test]
    fn vs_usual_mark_under_a_second_of_difference_is_always_usual() {
        let stats = StepStats {
            count: 5,
            busy_p50_s: Some(0.01),
            busy_p10_s: Some(0.005),
            busy_p90_s: Some(0.02),
            ..Default::default()
        };
        assert_eq!(vs_usual_mark(0.03, &stats, false), Some(VsUsualMark::Usual));
        assert_eq!(vs_usual_mark(0.0, &stats, false), Some(VsUsualMark::Usual));
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
        let footer = zoom_agent_footer(Some(14), Some(31), &tools, 375.0, None);
        assert_eq!(footer.turns, Some(14));
        assert_eq!(footer.tool_calls, Some(31));
        let (name, ms, pct) = footer.busiest_tool.unwrap();
        assert_eq!(name, "bash");
        assert_eq!(ms, 250_000);
        assert!((pct - 66.67).abs() < 0.1);
    }

    /// The footer must count the call in progress's own elapsed
    /// time, not just already-finished calls -- otherwise it reads
    /// `bash 0.0s` for seconds while the very first call of a run is
    /// still going.
    #[test]
    fn zoom_agent_footer_folds_the_running_calls_own_elapsed_time_in() {
        let tools = BTreeMap::new();
        let footer = zoom_agent_footer(
            Some(1),
            Some(1),
            &tools,
            5.0,
            Some(("bash".to_string(), 3_000)),
        );
        let (name, ms, _) = footer.busiest_tool.unwrap();
        assert_eq!(name, "bash");
        assert_eq!(ms, 3_000);
    }

    #[test]
    fn running_tool_elapsed_ms_reads_from_an_unmatched_transcript_call() {
        let calls = vec![ToolCall {
            start_t: Some("2026-01-01T00:00:10Z".to_string()),
            end_t: None,
            tool: "bash".to_string(),
            target: None,
            ok: None,
            ms: None,
            lines_added: None,
            lines_removed: None,
            stopped: false,
        }];
        let result = running_tool_elapsed_ms(&calls, None, 15.0, "2026-01-01T00:00:00Z");
        assert_eq!(result, Some(("bash".to_string(), 5_000)));
    }

    #[test]
    fn running_tool_elapsed_ms_falls_back_to_current_tool_with_no_transcript() {
        let result = running_tool_elapsed_ms(
            &[],
            Some(("read", "2026-01-01T00:00:10Z")),
            15.0,
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(result, Some(("read".to_string(), 5_000)));
    }

    #[test]
    fn running_tool_elapsed_ms_is_none_with_nothing_running() {
        assert_eq!(
            running_tool_elapsed_ms(&[], None, 15.0, "2026-01-01T00:00:00Z"),
            None
        );
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

    /// `expand_named_loop`'s own `BTreeMap::range` scan (its own doc
    /// comment) leans on `iter_9`'s own children sorting strictly
    /// before `iter_10`'s -- a naive string-prefix check alone would
    /// also have `iter_1` match `iter_10`'s children, so this pins
    /// the double-digit boundary a smaller fixture never reaches.
    #[test]
    fn expand_named_loop_tells_double_digit_pass_indices_apart() {
        use serde_json::json;
        let mut prime = serde_json::Map::new();
        for i in [1, 9, 10, 11] {
            prime.insert(
                format!("iter_{i}"),
                json!({"value": null, "meta": {
                    "created_at": format!("2026-01-01T00:00:{i:02}Z"),
                    "completed_at": format!("2026-01-01T00:00:{:02}Z", i + 1)
                }}),
            );
        }
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}, "items": serde_json::Value::Object(prime)}});
        let flat = crate::model::flatten_state(&state);
        let model = RunModel::new();
        let passes = expand_loop(
            "prime.items",
            &flat,
            &model,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:01:00Z",
        );
        assert_eq!(passes.len(), 4);
        assert_eq!(passes[0].label, "pass 2", "iter_1, 1-based");
        assert_eq!(passes[1].label, "pass 10", "iter_9");
        assert_eq!(passes[2].label, "pass 11", "iter_10");
        assert_eq!(passes[3].label, "pass 12", "iter_11");
        for pass in &passes {
            assert_eq!(
                pass.duration_s,
                Some(1.0),
                "each pass is its own one-second span, not merged with a neighbour's"
            );
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
    fn an_unnamed_loops_progress_shows_unknown_total_while_a_pass_runs() {
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
        let header = header_with(Some("2026-01-01T00:00:00Z"), None);
        let rows = vec![tool_row("prime.u_nap")];
        let time_rows = build_time_rows(&rows, &flat, &model, &plan, &header, &BTreeMap::new());
        let progress = time_rows[0]
            .loop_progress
            .expect("an unnamed loop with logged passes has progress");
        assert_eq!(
            progress.done, 1,
            "the pass in progress must not count as done"
        );
        assert_eq!(
            progress.total, None,
            "the total is unknown while the loop is still running"
        );

        model.observe_event(&crate::observe::Event::End {
            ts: "2026-01-01T00:00:12Z".to_string(),
            id: Some(2),
            path: "prime.u_nap".to_string(),
            ok: true,
            ms: Some(7_000),
            error: None,
        });
        let mut done_row = tool_row("prime.u_nap");
        done_row.status.kind = StatusKind::Done;
        let time_rows =
            build_time_rows(&[done_row], &flat, &model, &plan, &header, &BTreeMap::new());
        let progress = time_rows[0]
            .loop_progress
            .expect("an unnamed loop with logged passes has progress");
        assert_eq!(progress.done, 2);
        assert_eq!(
            progress.total,
            Some(2),
            "once the loop ends, the total is the real count"
        );
    }

    #[test]
    fn expand_unnamed_loop_lists_the_pass_in_progress_as_a_running_row() {
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
        let passes = expand_loop(
            "prime.u_nap",
            &BTreeMap::new(),
            &model,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:08Z",
        );
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].label, "#1");
        assert_eq!(passes[0].status.kind, StatusKind::Done);
        assert_eq!(passes[1].label, "#2");
        assert_eq!(passes[1].status.kind, StatusKind::Running);
        assert_eq!(passes[1].start_s, Some(5.0));
        assert_eq!(passes[1].duration_s, Some(3.0));
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
