//! The Combined Signal (`u` on All, design v1 part 5, issue #698):
//! every running run on one trace, stacked by run in launch order,
//! sent tokens above the axis and received below it, with one
//! legend row per run, cross-run Totals, a Per Run table and
//! Readings -- all terminal- and IO-independent, so a test builds
//! its own inputs with no real run or terminal involved. `tui.rs`
//! is the only thing that ever turns this into drawn cells.

use std::collections::{BTreeMap, BTreeSet};

use crate::format::format_share_pct;
use crate::model::{NodeMeta, RunModel, StatusKind};
use crate::observe::duration_seconds;
use crate::plan::PlanTree;
use crate::render::{Header, Row, RowKind};
use crate::screens::{SignalLastFinished, last_finished_row, tape_row_label};

/// The Combined Signal's own fixed column width, 100 ms, never
/// zoomed (its own key bar has no `+`/`-`).
pub const COLUMN_S: f64 = 0.1;

/// `offset_s` (seconds since the shared anchor) to its own 100 ms
/// column -- the same `floor` rule every other screen's own column
/// math already uses.
pub fn column_of(offset_s: f64) -> usize {
    (offset_s / COLUMN_S).floor().max(0.0) as usize
}

/// The trace's own floor (issue #713): 5 rows above the axis, the
/// axis itself, 3 below -- the smallest split that still keeps
/// both sides legible.
pub const TRACE_SENT_ROWS_MIN: usize = 5;
pub const TRACE_RECEIVED_ROWS_MIN: usize = 3;
/// The trace's own size at the frame's own 180x48 golden, and the
/// most the legend and bottom block ever leave it before it must
/// grow into extra height instead (extra height goes to the trace).
pub const TRACE_SENT_ROWS_BASE: usize = 14;
pub const TRACE_RECEIVED_ROWS_BASE: usize = 4;
/// The legend's own block at the golden, enough for up to 8 runs
/// without the chart needing to grow for it; past 8 runs the block
/// grows one row per run beyond it instead.
pub const LEGEND_ROWS_BASE: usize = 8;
/// The bottom block's (Per Run and Readings') own floor -- Per
/// Run's own header plus up to 3 scrollable rows, or Readings' own
/// 4 lines, each with its own border, whichever needs more (always
/// 6: Readings never needs fewer) -- and the most it ever grows to
/// (the golden's own size): it stays there even with more height
/// than the golden, all of which goes to the trace instead.
pub const LOWER_ROWS_MIN: usize = 6;
pub const LOWER_ROWS_MAX: usize = 13;
/// The chart panel's own rows that never shrink or grow with any
/// of the above: the seconds row, the Totals row itself, and the
/// panel's own two border rows. The separator row above Totals is
/// one more than this, but not required to fit at all -- the one
/// row [`combined_vertical_split`] drops first, before the trace
/// ever shrinks past its own base, so a run count large enough to
/// leave no other slack still keeps every legend row and Totals
/// itself, just with the divider between them gone.
const CHART_FIXED_ROWS: usize = 4;

/// One way to split the Combined Signal's own vertical space
/// (issue #713): how many rows the trace's own sent/received
/// sides and the legend get inside the chart panel, how many of
/// those are the separator row above Totals (0 or 1), and how
/// many rows the bottom block (Per Run and Readings) gets below
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombinedVerticalSplit {
    pub sent_rows: usize,
    pub received_rows: usize,
    pub legend_rows: usize,
    pub separator_rows: usize,
    pub lower_rows: usize,
}

impl CombinedVerticalSplit {
    /// The trace's own total rows, axis included.
    pub fn trace_rows(&self) -> usize {
        self.sent_rows + 1 + self.received_rows
    }

    /// The whole chart panel's own rows: the trace, the seconds
    /// row, the legend, the separator (if any), Totals, and the
    /// panel's own two border rows.
    pub fn chart_rows(&self) -> usize {
        self.trace_rows() + self.legend_rows + self.separator_rows + CHART_FIXED_ROWS
    }
}

/// The chart panel's own floor at `run_count` runs: the trace at
/// its own floor, every run's own legend row (never fewer), and
/// the panel's own fixed rows -- without the separator, the one
/// row this floor never needs.
pub fn combined_chart_rows_min(run_count: usize) -> usize {
    TRACE_SENT_ROWS_MIN + 1 + TRACE_RECEIVED_ROWS_MIN + CHART_FIXED_ROWS + run_count
}

/// The Combined Signal's own floor at `run_count` runs: the chart
/// panel's own floor plus the bottom block's own floor -- the rows
/// [`combined_vertical_split`] needs to return anything at all.
pub fn combined_body_rows_min(run_count: usize) -> usize {
    combined_chart_rows_min(run_count) + LOWER_ROWS_MIN
}

/// Splits `body_rows` (the chart panel and the bottom block
/// together, with no gap between them) between them at
/// `run_count` runs, following issue #713's own shrink order in
/// reverse (each one restored in the order the frame's own shrink
/// rule gives it up last): the separator row first (the one row
/// dropped before anything else in the list below), then the
/// bottom block's own blank rows (up to [`LOWER_ROWS_MAX`]), then
/// the legend's own blank rows (up to [`LEGEND_ROWS_BASE`], or
/// straight to `run_count` once that's already past it), then the
/// trace (up to its own [`TRACE_SENT_ROWS_BASE`]/
/// [`TRACE_RECEIVED_ROWS_BASE`], and past them with no further
/// cap). `None` below [`combined_body_rows_min`], the caller's own
/// too-small bounce.
pub fn combined_vertical_split(
    body_rows: usize,
    run_count: usize,
) -> Option<CombinedVerticalSplit> {
    let min_total = combined_body_rows_min(run_count);
    if body_rows < min_total {
        return None;
    }
    let mut slack = body_rows - min_total;

    let separator_rows = slack.min(1);
    slack -= separator_rows;

    let lower_room = LOWER_ROWS_MAX - LOWER_ROWS_MIN;
    let lower_grow = slack.min(lower_room);
    slack -= lower_grow;
    let lower_rows = LOWER_ROWS_MIN + lower_grow;

    let legend_base = run_count.max(LEGEND_ROWS_BASE);
    let legend_room = legend_base - run_count;
    let legend_grow = slack.min(legend_room);
    slack -= legend_grow;
    let legend_rows = run_count + legend_grow;

    let received_room = TRACE_RECEIVED_ROWS_BASE - TRACE_RECEIVED_ROWS_MIN;
    let sent_room = TRACE_SENT_ROWS_BASE - TRACE_SENT_ROWS_MIN;
    let trace_room = received_room + sent_room;
    let trace_grow = slack.min(trace_room);
    slack -= trace_grow;
    let received_grow = trace_grow.min(received_room);
    let sent_grow = (trace_grow - received_grow).min(sent_room);
    // Any slack past both the trace's own 180x48 bases (only once
    // `body_rows` is taller than the golden) goes entirely to the
    // sent side, the one side this screen has no fixed ceiling for.
    let sent_rows = TRACE_SENT_ROWS_MIN + sent_grow + slack;
    let received_rows = TRACE_RECEIVED_ROWS_MIN + received_grow;

    Some(CombinedVerticalSplit {
        sent_rows,
        received_rows,
        legend_rows,
        separator_rows,
        lower_rows,
    })
}

/// One run's own sent/received totals, bucketed by the *shared*
/// column (every run's own columns anchored to the earliest run's
/// own start, not each run's own) -- built once per redraw from the
/// same `Row`/flattened-state pair every other screen already
/// builds its own model from. A running prompt's own live estimate
/// lands at `now`'s own column, the same mutually-exclusive rule
/// `screens::build_signal`'s own bucket loop already follows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CombinedBuckets {
    pub sent: BTreeMap<usize, u64>,
    pub received: BTreeMap<usize, u64>,
    /// The column and real timestamp of whichever addition to
    /// `sent`/`received` landed furthest to the right -- Quiet
    /// reads the exact elapsed time to this, rather than rounding
    /// it to whole 100 ms columns the way the chart itself does.
    pub latest_activity: Option<(usize, String)>,
}

fn add(map: &mut BTreeMap<usize, u64>, col: usize, value: u64) {
    if value == 0 {
        return;
    }
    *map.entry(col).or_insert(0) += value;
}

fn note_activity(latest: &mut Option<(usize, String)>, col: usize, ts: &str) {
    if latest.as_ref().is_none_or(|(c, _)| col >= *c) {
        *latest = Some((col, ts.to_string()));
    }
}

/// [`CombinedBuckets`] for one run, anchored to `anchor_ts` (the
/// earliest of every run's own `run_start_ts`) rather than this
/// run's own start -- the reason this can't simply reuse a tile's
/// own [`crate::screens::SignalScreen::buckets`], which are anchored
/// to that run alone.
pub fn combined_run_buckets(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    model: &RunModel,
    anchor_ts: &str,
    now: &str,
) -> CombinedBuckets {
    let elapsed_offset = |ts: &str| duration_seconds(anchor_ts, ts);
    let mut out = CombinedBuckets::default();
    for row in rows {
        if !matches!(row.kind, RowKind::Prompt | RowKind::Reflector) {
            continue;
        }
        let node = flat.get(&row.path);
        let is_running = matches!(
            row.status.kind,
            StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
        );
        if is_running {
            let Some(live) = crate::model::prompt_live_figures(node, &row.path, model, now) else {
                continue;
            };
            let Some(offset) = elapsed_offset(now) else {
                continue;
            };
            let col = column_of(offset);
            let mut had_any = false;
            if let Some((sent, _)) = live.tokens_sent_or_estimate() {
                add(&mut out.sent, col, sent);
                had_any = sent > 0;
            }
            if let Some((received, _)) = live.tokens_received_or_estimate() {
                add(&mut out.received, col, received);
                had_any = had_any || received > 0;
            }
            if had_any {
                note_activity(&mut out.latest_activity, col, now);
            }
        } else if matches!(
            row.status.kind,
            StatusKind::Done | StatusKind::FailedHandled
        ) {
            let Some(node) = node else { continue };
            let Some(ts) = node.completed_at.as_deref() else {
                continue;
            };
            let Some(offset) = elapsed_offset(ts) else {
                continue;
            };
            let col = column_of(offset);
            let (sent, received) = crate::model::node_tokens(&node.meta);
            add(&mut out.sent, col, sent);
            add(&mut out.received, col, received);
            if sent + received > 0 {
                note_activity(&mut out.latest_activity, col, ts);
            }
        }
    }
    for sighting in model.agent_live_sightings() {
        let Some(offset) = elapsed_offset(&sighting.ts) else {
            continue;
        };
        let col = column_of(offset);
        add(&mut out.sent, col, sighting.tokens_sent);
        add(&mut out.received, col, sighting.tokens_received);
        if sighting.tokens_sent + sighting.tokens_received > 0 {
            note_activity(&mut out.latest_activity, col, &sighting.ts);
        }
    }
    out
}

/// The largest stacked column across every run's own `buckets`
/// (the shared scale is sized off this, sent and received
/// separately) -- every column any run has ever touched, not only
/// the columns a single run happens to have.
pub fn largest_stacked_column(buckets: &[&BTreeMap<usize, u64>]) -> u64 {
    let mut cols: BTreeSet<usize> = BTreeSet::new();
    for b in buckets {
        cols.extend(b.keys().copied());
    }
    cols.iter()
        .map(|&c| {
            buckets
                .iter()
                .map(|b| b.get(&c).copied().unwrap_or(0))
                .sum()
        })
        .max()
        .unwrap_or(0)
}

/// `frac` in `(0, 1]`, the same rounding
/// [`crate::screens`]'s single-run Signal chart already uses for a
/// sent bar's own partial top cell -- kept here too since a stacked
/// column's own partial cap uses the identical eighths scale.
pub fn sent_partial_glyph(frac: f64) -> char {
    const GLYPHS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let level = (frac.clamp(0.0, 1.0) * 8.0).round() as usize;
    GLYPHS[level.saturating_sub(1).min(7)]
}

/// A received bar's own partial cell, upper-anchored the same way
/// the single-run chart's own received column already rounds one.
pub fn received_partial_glyph(frac: f64) -> char {
    if frac >= 0.75 {
        '\u{2588}'
    } else if frac >= 0.3125 {
        '\u{2580}'
    } else {
        '\u{2594}'
    }
}

/// One cell of a stacked column: which of `values`' own run (by
/// index) drew it, and its glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackedCell {
    pub run_index: usize,
    pub glyph: char,
}

/// A stacked column's own `rows` cells, drawn from the axis upward,
/// in `values`' own order (the callers' launch order, bottom to
/// top): each run's own cumulative share fills whole rows first,
/// the row a run's own cumulative total first reaches credited to
/// that run; the final, possibly partial, row goes to whichever run
/// contributes the largest share of it, never simply the last one
/// processed (the full-row loop below reasons the same way, not
/// just this one row). `partial_glyph` picks `sent_partial_glyph`/
/// `received_partial_glyph` for the caller's own side of the axis.
pub fn stack_column(
    values: &[u64],
    axis_top: u64,
    rows: usize,
    partial_glyph: fn(f64) -> char,
) -> Vec<Option<StackedCell>> {
    let mut out = vec![None; rows];
    if axis_top == 0 || rows == 0 {
        return out;
    }
    let unit = axis_top as f64 / rows as f64;
    let mut cumulative = 0u64;
    let mut filled = 0usize;
    let mut last_nonzero = None;
    for (run_index, &value) in values.iter().enumerate() {
        if value == 0 {
            continue;
        }
        last_nonzero = Some(run_index);
        let carry = (cumulative as f64) % unit;
        cumulative += value;
        let full_total = ((cumulative as f64 / unit).floor() as usize).min(rows);
        // The first row this run's own addition newly completes,
        // when the cumulative total just before it wasn't itself an
        // exact multiple of the row unit, is shared with whatever
        // an earlier run already left in it -- drawn one eighth
        // short of solid to mark that blend, rather than solid as
        // every row after it (completed by this run alone) is.
        let mixed_first_row = filled < full_total && carry > 0.0001;
        for j in filled..full_total {
            let glyph = if mixed_first_row && j == filled {
                partial_glyph(7.0 / 8.0)
            } else {
                '\u{2588}'
            };
            out[rows - 1 - j] = Some(StackedCell { run_index, glyph });
        }
        filled = full_total;
    }
    if filled < rows {
        let remainder = cumulative as f64 - filled as f64 * unit;
        if remainder > 0.0 {
            // Unlike a full row (credited to whichever run's own
            // addition reached it, above), the one partial cap row
            // can hold more than one run's own leftover at once (a
            // column whose combined total never even fills one
            // whole row) -- credited to whichever contributes the
            // larger *share of this one row*, not simply the last
            // one processed.
            let row_start = filled as f64 * unit;
            let mut best: Option<(usize, f64)> = None;
            let mut seen = 0u64;
            for (run_index, &value) in values.iter().enumerate() {
                if value == 0 {
                    continue;
                }
                let start = seen as f64;
                let end = (seen + value) as f64;
                let overlap = (end.min(cumulative as f64) - start.max(row_start)).max(0.0);
                if overlap > 0.0 && best.is_none_or(|(_, b)| overlap > b) {
                    best = Some((run_index, overlap));
                }
                seen += value;
            }
            if let Some(run_index) = best.map(|(i, _)| i).or(last_nonzero) {
                out[rows - 1 - filled] = Some(StackedCell {
                    run_index,
                    glyph: partial_glyph(remainder / unit),
                });
            }
        }
    }
    out
}

/// Every running leaf's own label, in plan order -- the Per Run
/// table's own NOW column and the legend's own running label both
/// read this list, joined and cut differently for the room each has.
pub fn running_step_labels(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
) -> Vec<String> {
    rows.iter()
        .filter(|r| {
            r.kind.is_leaf()
                && matches!(
                    r.status.kind,
                    StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                )
        })
        .map(|r| tape_row_label(r, flat, plan))
        .collect()
}

/// `steps` joined by ` · `, cut to the widest prefix that fits
/// `max_width` columns with a trailing `+N` for the rest (the Per
/// Run table's own NOW column, the legend's own running label) --
/// never cuts a step's own text short, only drops whole steps from
/// the end. Every step shown when they all fit, with no `+0`.
pub fn join_steps_with_overflow(steps: &[String], max_width: usize) -> String {
    if steps.is_empty() {
        return String::new();
    }
    let full = steps.join(" \u{b7} ");
    if full.chars().count() <= max_width {
        return full;
    }
    let mut shown = 0usize;
    let mut text = String::new();
    for (i, step) in steps.iter().enumerate() {
        let candidate = if i == 0 {
            step.clone()
        } else {
            format!("{text} \u{b7} {step}")
        };
        let remaining = steps.len() - i - 1;
        let suffix_len = if remaining > 0 {
            format!(" +{remaining}").chars().count()
        } else {
            0
        };
        if candidate.chars().count() + suffix_len > max_width && shown > 0 {
            break;
        }
        text = candidate;
        shown = i + 1;
    }
    let remaining = steps.len() - shown;
    if remaining > 0 {
        format!("{text} +{remaining}")
    } else {
        text
    }
}

/// The first finished prompt or reflector by its own real finish
/// time, how many more have finished, and its own combined column
/// (for the legend's own placement) -- `None` with nothing finished
/// yet.
pub fn finished_prompt_first_and_extra(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    anchor_ts: &str,
) -> Option<(String, usize, usize)> {
    let gather = |kinds: &[RowKind]| -> Vec<(i64, usize, &Row)> {
        let mut finishes: Vec<(i64, usize, &Row)> = rows
            .iter()
            .filter(|r| kinds.contains(&r.kind))
            .filter(|r| matches!(r.status.kind, StatusKind::Done | StatusKind::FailedHandled))
            .filter_map(|r| {
                let node = flat.get(&r.path)?;
                let ts = node.completed_at.as_deref()?;
                let parsed = crate::diff::parse_instant(ts)?.timestamp_millis();
                let offset = duration_seconds(anchor_ts, ts)?;
                Some((parsed, column_of(offset), r))
            })
            .collect();
        finishes.sort_by_key(|(ts, _, _)| *ts);
        finishes
    };
    // A run with no finished prompt or reflector at all -- every
    // one of its own effects a tool step -- falls back to its
    // finished tool steps instead, the same widening issue #702
    // already gave the single-run Signal's own Largest reading, so
    // such a run still shows something finished here. A run with
    // any finished prompt or reflector keeps reading only those, so
    // a tool step that finishes earlier never pre-empts the frame's
    // own choice.
    let finishes = {
        let prompts = gather(&[RowKind::Prompt, RowKind::Reflector]);
        if prompts.is_empty() {
            gather(&[RowKind::Tool])
        } else {
            prompts
        }
    };
    let total = finishes.len();
    if total == 0 {
        return None;
    }
    let (_, col, first) = finishes[0];
    Some((tape_row_label(first, flat, plan), total - 1, col))
}

/// One run's own row everywhere the Combined Signal shows one: the
/// chart's own legend row, the Per Run table, and its own
/// contribution to the cross-run Totals and Readings.
pub struct CombinedRun {
    pub role: usize,
    pub document_label: String,
    pub config_label: Option<String>,
    pub engine_label: String,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub tokens_total: u64,
    pub effects_done: u64,
    pub effects_total: Option<u64>,
    pub running: Vec<String>,
    pub finished_label: Option<String>,
    pub finished_extra: usize,
    pub finished_col: Option<usize>,
    pub buckets: CombinedBuckets,
    pub last_finished: Option<SignalLastFinished>,
    /// This run's own share of every run's tokens (the Per Run
    /// table's own SHARE column, the Readings' own Largest line) --
    /// filled in once every run's own total is known, so `0.0`
    /// until [`build_combined_signal`] has computed it.
    pub share_pct: f64,
}

/// One input the Combined Signal builds one [`CombinedRun`] from --
/// the same `Row`/flattened-state/`Header` triple a tile's own
/// Signal screen already builds from, built fresh for every run
/// (regardless of whichever screen the All grid's own tiles happen
/// to be showing) since the Combined Signal needs every run's own
/// Signal data at once.
pub struct CombinedRunInput<'a> {
    pub document_label: String,
    pub config_label: Option<String>,
    pub engine_label: String,
    /// Unfolded: a loop with several passes folds into one
    /// `QuickSteps` summary row in `rows`, dropping the individual
    /// rows this screen's own finished-prompt and running-step
    /// scans need.
    pub rows: &'a [Row],
    pub flat: &'a BTreeMap<String, NodeMeta>,
    pub plan: &'a PlanTree,
    pub model: &'a RunModel,
    pub header: &'a Header,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CombinedTotals {
    pub sent: u64,
    pub received: u64,
    pub total: u64,
    pub run_count: usize,
    pub peak_tokens: u64,
    pub peak_time_s: f64,
    pub rate_tokens_per_s: f64,
    pub quiet_s: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CombinedReadings {
    pub run_count: usize,
    pub engine_count: usize,
    pub steps_running: usize,
    pub largest_doc: Option<String>,
    pub largest_tokens: u64,
    pub largest_share_pct: f64,
}

pub struct CombinedSignalScreen {
    pub sent_axis_top: u64,
    pub received_axis_top: u64,
    pub runs: Vec<CombinedRun>,
    pub now_col: usize,
    pub now_elapsed_s: f64,
    pub totals: CombinedTotals,
    pub readings: CombinedReadings,
}

/// Every run's own `run_start_ts`'s earliest -- the Combined
/// Signal's own shared time anchor, the trace's own column 0.
/// `None` with no run started yet (never actually reached: the All
/// screen only ever enters this screen with at least one running
/// run already known).
pub fn earliest_run_start<'a>(starts: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    starts.into_iter().min_by(|a, b| {
        let pa = crate::diff::parse_instant(a).map(|t| t.timestamp_millis());
        let pb = crate::diff::parse_instant(b).map(|t| t.timestamp_millis());
        pa.cmp(&pb)
    })
}

/// Builds the whole Combined Signal (issue #698): `inputs` in launch
/// order (the same order the legend, the Per Run table and the
/// chart's own stacking all share), `anchor_ts` already resolved by
/// [`earliest_run_start`], `floor_sent`/`floor_received` the shared
/// scale's own persisted top so far (never sliding back down).
pub fn build_combined_signal(
    inputs: &[CombinedRunInput<'_>],
    anchor_ts: &str,
    floor_sent: u64,
    floor_received: u64,
    now: &str,
    cursor_col: Option<usize>,
) -> CombinedSignalScreen {
    let now_offset = duration_seconds(anchor_ts, now).unwrap_or(0.0);
    let now_col = column_of(now_offset);
    // Peak and Quiet read only up to the cursor's own column (Now's,
    // when not scrubbed), the same moment a single run's own Signal
    // stops reading its own Readings at once scrubbed away from Now.
    let view_col = cursor_col.unwrap_or(now_col);

    let mut runs: Vec<CombinedRun> = Vec::with_capacity(inputs.len());
    let mut engines: BTreeSet<String> = BTreeSet::new();
    let mut steps_running = 0usize;
    for (role, input) in inputs.iter().enumerate() {
        let buckets = combined_run_buckets(input.rows, input.flat, input.model, anchor_ts, now);
        let running = running_step_labels(input.rows, input.flat, input.plan);
        steps_running += running.len();
        let (finished_label, finished_extra, finished_col) =
            match finished_prompt_first_and_extra(input.rows, input.flat, input.plan, anchor_ts) {
                Some((label, extra, col)) => (Some(label), extra, Some(col)),
                None => (None, 0, None),
            };
        let tokens_sent = input.header.tokens_sent + input.header.tokens_sent_live_estimate;
        let tokens_received =
            input.header.tokens_received + input.header.tokens_received_live_estimate;
        let last_finished = last_finished_row(
            input.rows,
            input.flat,
            input.plan,
            input.header.run_start_ts.as_deref(),
        );
        engines.insert(
            input
                .engine_label
                .split_whitespace()
                .next()
                .unwrap_or(&input.engine_label)
                .to_string(),
        );
        runs.push(CombinedRun {
            role,
            document_label: input.document_label.clone(),
            config_label: input.config_label.clone(),
            engine_label: input.engine_label.clone(),
            tokens_sent,
            tokens_received,
            tokens_total: tokens_sent + tokens_received,
            effects_done: input.header.effects_done,
            effects_total: input
                .header
                .effects_planned_full
                .or(input.header.effects_planned),
            running,
            finished_label,
            finished_extra,
            finished_col,
            buckets,
            last_finished,
            share_pct: 0.0,
        });
    }

    let grand_total: u64 = runs.iter().map(|r| r.tokens_total).sum();
    for run in &mut runs {
        run.share_pct = if grand_total > 0 {
            run.tokens_total as f64 / grand_total as f64 * 100.0
        } else {
            0.0
        };
    }

    let sent_buckets: Vec<&BTreeMap<usize, u64>> = runs.iter().map(|r| &r.buckets.sent).collect();
    let received_buckets: Vec<&BTreeMap<usize, u64>> =
        runs.iter().map(|r| &r.buckets.received).collect();
    let largest_sent = largest_stacked_column(&sent_buckets);
    let largest_received = largest_stacked_column(&received_buckets);
    let sent_axis_top = crate::multi::shared_signal_axis_top(largest_sent, floor_sent);
    let received_axis_top = crate::multi::shared_signal_axis_top(largest_received, floor_received);

    let mut peak_col = 0usize;
    let mut peak_tokens = 0u64;
    let mut all_cols: BTreeSet<usize> = BTreeSet::new();
    for b in sent_buckets.iter().chain(received_buckets.iter()) {
        all_cols.extend(b.keys().copied());
    }
    for &col in all_cols.iter().filter(|&&col| col <= view_col) {
        let sent_here: u64 = sent_buckets
            .iter()
            .map(|b| b.get(&col).copied().unwrap_or(0))
            .sum();
        if sent_here > peak_tokens {
            peak_tokens = sent_here;
            peak_col = col;
        }
        let received_here: u64 = received_buckets
            .iter()
            .map(|b| b.get(&col).copied().unwrap_or(0))
            .sum();
        if received_here > peak_tokens {
            peak_tokens = received_here;
            peak_col = col;
        }
    }

    let sent_total: u64 = runs.iter().map(|r| r.tokens_sent).sum();
    let received_total: u64 = runs.iter().map(|r| r.tokens_received).sum();
    let rate = if now_offset > 0.0 {
        grand_total as f64 / now_offset
    } else {
        0.0
    };
    // Quiet is the time since the newest column with anything in
    // it, up to the cursor -- it follows the scrub cursor the same
    // way Peak does, rather than the newest *finished step* across
    // every run (which a tool step with no tokens at all could move
    // without anything showing on the trace itself).
    let quiet_s = runs
        .iter()
        .filter_map(|r| r.buckets.latest_activity.as_ref())
        .filter(|(col, _)| *col <= view_col)
        .max_by_key(|(col, ts)| (*col, ts.clone()))
        .and_then(|(_, ts)| duration_seconds(ts, now));

    let totals = CombinedTotals {
        sent: sent_total,
        received: received_total,
        total: grand_total,
        run_count: runs.len(),
        peak_tokens,
        peak_time_s: peak_col as f64 * COLUMN_S,
        rate_tokens_per_s: rate,
        quiet_s,
    };

    // A tie goes to the first in launch order, not whichever
    // `Iterator::max_by_key` happens to keep last among equal
    // values.
    let largest_run = runs
        .iter()
        .fold(None, |best: Option<&CombinedRun>, r| match best {
            Some(b) if r.tokens_total <= b.tokens_total => best,
            _ => Some(r),
        });
    let readings = CombinedReadings {
        run_count: runs.len(),
        engine_count: engines.len(),
        steps_running,
        largest_doc: largest_run.map(|r| r.document_label.clone()),
        largest_tokens: largest_run.map(|r| r.tokens_total).unwrap_or(0),
        largest_share_pct: largest_run.map(|r| r.share_pct).unwrap_or(0.0),
    };

    CombinedSignalScreen {
        sent_axis_top,
        received_axis_top,
        runs,
        now_col,
        now_elapsed_s: now_offset,
        totals,
        readings,
    }
}

/// The Per Run table's own SHARE column and the Readings' own
/// Largest line, `<1%` below one point (§3.3's own rounding, reused
/// rather than a second copy of it).
pub fn format_share(share_pct: f64) -> String {
    format_share_pct(share_pct)
}

/// A legend row's own finished-prompt label: `label`/`extra`
/// first, the run's own tokens so far appended only while the
/// whole thing still ends strictly before `limit_col` (the running
/// label's own column, or the panel's own right edge with nothing
/// running) -- the token figure is always the first thing a cut
/// label loses, never the name or the `+N`.
pub fn finished_legend_text(
    label: &str,
    extra: usize,
    tokens_total: u64,
    start_col: usize,
    limit_col: usize,
) -> String {
    let extra_text = if extra > 0 {
        format!(" +{extra}")
    } else {
        String::new()
    };
    let with_tokens = format!(
        "\u{2713} {label}{extra_text} \u{b7} {}",
        crate::format::format_tokens(tokens_total)
    );
    if start_col + with_tokens.chars().count() < limit_col {
        return with_tokens;
    }
    format!("\u{2713} {label}{extra_text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_vertical_split_at_120x30_with_3_runs_gives_every_legend_row_and_a_header_plus_three_table_rows()
     {
        // 120x30: 30 rows total, less the header band, status line
        // and key bar leaves 27 body rows.
        let split = combined_vertical_split(27, 3).expect("fits above the floor");
        assert_eq!(split.legend_rows, 3);
        assert_eq!(split.separator_rows, 1);
        // The bottom block's own floor (6) plus whatever slack is
        // left once the trace stays at its own floor and the
        // legend has no blanks to give up: a header row plus up to
        // 3 scrollable rows always fit inside it.
        assert!(split.lower_rows >= 6);
        assert_eq!(split.trace_rows(), 9);
    }

    #[test]
    fn combined_vertical_split_at_120x30_with_8_runs_keeps_every_legend_row_and_scrolls_the_table()
    {
        let split = combined_vertical_split(27, 8).expect("fits above the floor");
        assert_eq!(split.legend_rows, 8);
        // The bottom block stays at its own floor (6: a header row
        // plus 3 scrollable rows) -- fewer than the 8 runs, so the
        // Per Run table must scroll to keep the cursor in view.
        assert_eq!(split.lower_rows, 6);
        assert!(split.lower_rows - 3 < 8);
        // No slack left for the separator above Totals once every
        // run has its own legend row at this height.
        assert_eq!(split.separator_rows, 0);
    }

    #[test]
    fn combined_vertical_split_at_180x48_with_6_runs_matches_the_frame() {
        // 180x48: 45 body rows, the golden's own size.
        let split = combined_vertical_split(45, 6).expect("fits above the floor");
        assert_eq!(split.separator_rows, 1);
        assert_eq!(split.sent_rows, TRACE_SENT_ROWS_BASE);
        assert_eq!(split.received_rows, TRACE_RECEIVED_ROWS_BASE);
        assert_eq!(split.legend_rows, LEGEND_ROWS_BASE);
        assert_eq!(split.lower_rows, LOWER_ROWS_MAX);
        assert_eq!(split.chart_rows(), 32);
    }

    #[test]
    fn combined_vertical_split_at_187x61_grows_only_the_trace() {
        // 187x61: 58 body rows, more than the golden's own 45.
        let split = combined_vertical_split(58, 3).expect("fits above the floor");
        assert!(split.trace_rows() > 19);
        assert_eq!(split.legend_rows, LEGEND_ROWS_BASE);
        assert_eq!(split.lower_rows, LOWER_ROWS_MAX);
    }

    #[test]
    fn combined_vertical_split_fits_at_the_floor_and_falls_back_one_row_below_it() {
        let min = combined_body_rows_min(3);
        assert!(combined_vertical_split(min, 3).is_some());
        assert!(combined_vertical_split(min - 1, 3).is_none());
    }

    #[test]
    fn stack_column_puts_the_first_run_at_the_axis() {
        // Two runs, 1000 each, axis_top 4000 over 4 rows (unit
        // 1000/row): run0 fills row index0 (axis-adjacent), run1
        // fills row index1 right above it.
        let cells = stack_column(&[1000, 1000], 4000, 4, sent_partial_glyph);
        assert_eq!(cells[3].map(|c| c.run_index), Some(0));
        assert_eq!(cells[2].map(|c| c.run_index), Some(1));
        assert_eq!(cells[1], None);
        assert_eq!(cells[0], None);
    }

    #[test]
    fn stack_column_credits_a_full_row_to_whichever_run_completes_it() {
        // run0 alone doesn't reach row index0's own threshold
        // (1000); run1's addition does, and the total (1900) still
        // doesn't reach a second full row -- row index0 is credited
        // to run1, not run0, and the remainder becomes run1's own
        // partial cap.
        let cells = stack_column(&[600, 1300], 4000, 4, sent_partial_glyph);
        assert_eq!(cells[3].map(|c| c.run_index), Some(1));
        assert_eq!(
            cells[2].map(|c| (c.run_index, c.glyph)),
            Some((1, '\u{2587}'))
        );
    }

    #[test]
    fn stack_column_marks_a_shared_full_row_one_eighth_short_of_solid() {
        // run0's own 600 doesn't reach row index0's own threshold
        // (1000) by itself; run1's addition completes it, so that
        // row holds both runs' tokens, not run1's alone -- drawn
        // one eighth short of solid to mark the blend, unlike a row
        // a single run completes entirely on its own.
        let cells = stack_column(&[600, 1300], 4000, 4, sent_partial_glyph);
        assert_eq!(cells[3].map(|c| c.glyph), Some('\u{2587}'));
    }

    #[test]
    fn stack_column_never_marks_a_runs_own_first_row_as_shared() {
        // run0 starts the stack from the axis itself (nothing below
        // it to share with), so even though its own 1300 crosses
        // one row boundary, that row reads solid.
        let cells = stack_column(&[1300, 0], 4000, 4, sent_partial_glyph);
        assert_eq!(cells[3].map(|c| c.glyph), Some('\u{2588}'));
    }

    #[test]
    fn stack_column_credits_the_partial_cap_to_the_larger_share_even_when_processed_first() {
        // run0 (1300) is processed before run1 (100), and run0's
        // own 300 remaining after row index0 is larger than run1's
        // own 100 -- the cap goes to run0 even though run1 was the
        // last one added, not simply the last run processed.
        let cells = stack_column(&[1300, 100], 4000, 4, sent_partial_glyph);
        assert_eq!(
            cells[2].map(|c| (c.run_index, c.glyph)),
            Some((0, '\u{2583}'))
        );
    }

    #[test]
    fn stack_column_is_empty_with_nothing_in_the_column() {
        let cells = stack_column(&[0, 0], 4000, 4, sent_partial_glyph);
        assert!(cells.iter().all(Option::is_none));
    }

    #[test]
    fn largest_stacked_column_sums_every_runs_own_bucket() {
        let mut a = BTreeMap::new();
        a.insert(5usize, 200u64);
        let mut b = BTreeMap::new();
        b.insert(5usize, 300u64);
        b.insert(9usize, 50u64);
        assert_eq!(largest_stacked_column(&[&a, &b]), 500);
    }

    #[test]
    fn join_steps_with_overflow_shows_everything_that_fits() {
        let steps = vec!["tests".to_string()];
        assert_eq!(join_steps_with_overflow(&steps, 20), "tests");
    }

    #[test]
    fn join_steps_with_overflow_counts_what_does_not_fit() {
        let steps = vec![
            "iter_0 \u{203a} lint".to_string(),
            "iter_1 \u{203a} lint".to_string(),
            "iter_2 \u{203a} lint".to_string(),
        ];
        let joined = join_steps_with_overflow(&steps, 24);
        assert_eq!(joined, "iter_0 \u{203a} lint +2");
    }

    #[test]
    fn join_steps_with_overflow_never_shows_plus_zero() {
        let steps = vec!["a".to_string(), "b".to_string()];
        assert_eq!(join_steps_with_overflow(&steps, 100), "a \u{b7} b");
    }

    #[test]
    fn earliest_run_start_picks_the_smallest_timestamp() {
        let starts = ["2026-01-01T00:00:02Z", "2026-01-01T00:00:01Z"];
        assert_eq!(earliest_run_start(starts), Some("2026-01-01T00:00:01Z"));
    }

    #[test]
    fn finished_legend_text_keeps_tokens_when_there_is_room() {
        let text = finished_legend_text("plan", 3, 4_646, 22, 46);
        assert_eq!(text, "\u{2713} plan +3 \u{b7} 4.6 kt");
    }

    #[test]
    fn finished_legend_text_drops_tokens_before_the_running_column() {
        // Reaching exactly `limit_col` still loses the token figure,
        // at a boundary chosen for this test alone -- the frame's
        // own classify row sits at different columns and has room.
        let text = finished_legend_text("iter_0 \u{203a} label", 5, 1_152, 17, 45);
        assert_eq!(text, "\u{2713} iter_0 \u{203a} label +5");
    }

    fn combined_run_input<'a>(
        document_label: &str,
        header: &'a Header,
        rows: &'a [Row],
        flat: &'a BTreeMap<String, NodeMeta>,
        plan: &'a PlanTree,
        model: &'a RunModel,
    ) -> CombinedRunInput<'a> {
        CombinedRunInput {
            document_label: document_label.to_string(),
            config_label: None,
            engine_label: "cof".to_string(),
            rows,
            flat,
            plan,
            model,
            header,
        }
    }

    fn finished_prompt_state(
        tokens_sent: u64,
        tokens_received: u64,
        completed_at: &str,
    ) -> serde_json::Value {
        serde_json::json!({"prime": {"value": null, "meta": {"completed_at": completed_at},
            "step": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": completed_at,
                "tokens_sent": tokens_sent, "tokens_received": tokens_received, "adapter": "scripted", "model": "m"}}
        }})
    }

    #[test]
    fn build_combined_signal_computes_peak_rate_and_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("do.yml");
        std::fs::write(
            &path,
            "effects:\n  - name: step\n    type: prompt\n    template: hi\n",
        )
        .unwrap();
        let program = crate::plan::compile(&path, None, &indexmap::IndexMap::new()).unwrap();
        let plan = PlanTree::from_program(&program);
        let now = "2026-01-01T00:00:03Z";
        let anchor = "2026-01-01T00:00:00Z";

        let state_a = finished_prompt_state(1_000, 0, "2026-01-01T00:00:01Z");
        let flat_a = crate::model::flatten_state(&state_a);
        let rendered_a = crate::render::build_folded(
            "a.yml",
            "cof",
            &plan,
            &mut RunModel::new(),
            Some(&state_a),
            crate::model::ProcessState::Running,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let model_a = RunModel::new();

        let state_b = finished_prompt_state(500, 0, "2026-01-01T00:00:02Z");
        let flat_b = crate::model::flatten_state(&state_b);
        let rendered_b = crate::render::build_folded(
            "b.yml",
            "cof",
            &plan,
            &mut RunModel::new(),
            Some(&state_b),
            crate::model::ProcessState::Running,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let model_b = RunModel::new();

        let inputs = vec![
            combined_run_input(
                "a.yml",
                &rendered_a.header,
                &rendered_a.unfolded_rows,
                &flat_a,
                &plan,
                &model_a,
            ),
            combined_run_input(
                "b.yml",
                &rendered_b.header,
                &rendered_b.unfolded_rows,
                &flat_b,
                &plan,
                &model_b,
            ),
        ];
        let screen = build_combined_signal(&inputs, anchor, 0, 0, now, None);
        // a's 1,000 t at col10 (1.0s) is the peak; b's 500 t at
        // col20 (2.0s) isn't.
        assert_eq!(screen.totals.peak_tokens, 1_000);
        assert_eq!(screen.totals.peak_time_s, 1.0);
        // Rate: 1,500 t over the 3s since the anchor.
        assert!((screen.totals.rate_tokens_per_s - 500.0).abs() < 1.0);
        // Quiet: the newest finish (b, at 2.0s) is 1.0s before now (3.0s).
        assert!((screen.totals.quiet_s.unwrap() - 1.0).abs() < 0.01);
        // SHARE: a's 1,000 of 1,500 total is two thirds.
        assert!((screen.runs[0].share_pct - 66.666_66).abs() < 0.01);
        assert!((screen.runs[1].share_pct - 33.333_33).abs() < 0.01);
        assert_eq!(format_share(screen.runs[0].share_pct), "67%");
    }

    #[test]
    fn build_combined_signal_follows_the_scrub_cursor_for_peak_and_quiet() {
        // Same two runs as above (a finishes 1,000 t at 1.0s, b
        // finishes 500 t at 2.0s), but scrubbed back to a's own
        // column: b's finish is past the cursor, so it moves
        // neither Peak nor Quiet, the same as it would if it simply
        // hadn't happened yet.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("do.yml");
        std::fs::write(
            &path,
            "effects:\n  - name: step\n    type: prompt\n    template: hi\n",
        )
        .unwrap();
        let program = crate::plan::compile(&path, None, &indexmap::IndexMap::new()).unwrap();
        let plan = PlanTree::from_program(&program);
        let now = "2026-01-01T00:00:03Z";
        let anchor = "2026-01-01T00:00:00Z";

        let state_a = finished_prompt_state(1_000, 0, "2026-01-01T00:00:01Z");
        let flat_a = crate::model::flatten_state(&state_a);
        let rendered_a = crate::render::build_folded(
            "a.yml",
            "cof",
            &plan,
            &mut RunModel::new(),
            Some(&state_a),
            crate::model::ProcessState::Running,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let model_a = RunModel::new();

        let state_b = finished_prompt_state(500, 0, "2026-01-01T00:00:02Z");
        let flat_b = crate::model::flatten_state(&state_b);
        let rendered_b = crate::render::build_folded(
            "b.yml",
            "cof",
            &plan,
            &mut RunModel::new(),
            Some(&state_b),
            crate::model::ProcessState::Running,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let model_b = RunModel::new();

        let inputs = vec![
            combined_run_input(
                "a.yml",
                &rendered_a.header,
                &rendered_a.unfolded_rows,
                &flat_a,
                &plan,
                &model_a,
            ),
            combined_run_input(
                "b.yml",
                &rendered_b.header,
                &rendered_b.unfolded_rows,
                &flat_b,
                &plan,
                &model_b,
            ),
        ];
        let cursor_col = column_of(1.0); // a's own column (1.0s)
        let screen = build_combined_signal(&inputs, anchor, 0, 0, now, Some(cursor_col));
        assert_eq!(screen.totals.peak_tokens, 1_000);
        assert_eq!(screen.totals.peak_time_s, 1.0);
        // Quiet: a's own finish (1.0s) is 2.0s before now (3.0s),
        // not b's later one (which the cursor hides).
        assert!((screen.totals.quiet_s.unwrap() - 2.0).abs() < 0.01);
    }

    #[test]
    fn build_combined_signal_grows_the_shared_scale_and_never_shrinks_it() {
        // a alone stacks 1,000 t into one column: 1.5 x 1,000 =
        // 1,500, so the smallest round top at or above that is
        // 2,000.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("do.yml");
        std::fs::write(
            &path,
            "effects:\n  - name: step\n    type: prompt\n    template: hi\n",
        )
        .unwrap();
        let program = crate::plan::compile(&path, None, &indexmap::IndexMap::new()).unwrap();
        let plan = PlanTree::from_program(&program);
        let now = "2026-01-01T00:00:03Z";
        let anchor = "2026-01-01T00:00:00Z";
        let state_a = finished_prompt_state(1_000, 0, "2026-01-01T00:00:01Z");
        let flat_a = crate::model::flatten_state(&state_a);
        let rendered_a = crate::render::build_folded(
            "a.yml",
            "cof",
            &plan,
            &mut RunModel::new(),
            Some(&state_a),
            crate::model::ProcessState::Running,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let model_a = RunModel::new();
        let inputs = vec![combined_run_input(
            "a.yml",
            &rendered_a.header,
            &rendered_a.unfolded_rows,
            &flat_a,
            &plan,
            &model_a,
        )];

        let first = build_combined_signal(&inputs, anchor, 0, 0, now, None);
        assert_eq!(first.sent_axis_top, 2_000);

        // Fed back as the floor (the way `keys::AllState`'s own
        // `combined_sent_top` carries it from one redraw to the
        // next), the same data keeps the same top rather than
        // recomputing a smaller one from scratch.
        let second = build_combined_signal(&inputs, anchor, first.sent_axis_top, 0, now, None);
        assert_eq!(second.sent_axis_top, 2_000);

        // A floor from a larger column seen earlier persists even
        // though this data alone wouldn't justify it.
        let third = build_combined_signal(&inputs, anchor, 50_000, 0, now, None);
        assert_eq!(third.sent_axis_top, 50_000);
    }

    fn combined_tool_row(path: &str, label: &str, status: StatusKind) -> Row {
        Row {
            path: path.to_string(),
            depth: 1,
            label: label.to_string(),
            kind: RowKind::Tool,
            status: crate::model::RowStatus {
                kind: status,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: false,
        }
    }

    fn combined_tool_node(created_at: &str, completed_at: Option<&str>) -> NodeMeta {
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
            value: serde_json::Value::Null,
            meta: serde_json::json!({}),
        }
    }

    #[test]
    fn finished_prompt_first_and_extra_falls_back_to_a_finished_tool_with_no_prompt_at_all() {
        let plan = PlanTree::empty();
        let rows = vec![combined_tool_row(
            "prime.warmup",
            "warmup",
            StatusKind::Done,
        )];
        let mut flat = BTreeMap::new();
        flat.insert(
            "prime.warmup".to_string(),
            combined_tool_node("2026-01-01T00:00:00Z", Some("2026-01-01T00:00:01Z")),
        );
        let (label, extra, _col) =
            finished_prompt_first_and_extra(&rows, &flat, &plan, "2026-01-01T00:00:00Z")
                .expect("a finished tool step, with no prompt to prefer");
        assert_eq!(label, "warmup");
        assert_eq!(extra, 0);
    }

    #[test]
    fn finished_prompt_first_and_extra_prefers_a_finished_prompt_over_an_earlier_tool() {
        let plan = PlanTree::empty();
        let rows = vec![
            combined_tool_row("prime.warmup", "warmup", StatusKind::Done),
            Row {
                path: "prime.step".to_string(),
                depth: 1,
                label: "step".to_string(),
                kind: RowKind::Prompt,
                status: crate::model::RowStatus {
                    kind: StatusKind::Done,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: false,
            },
        ];
        let mut flat = BTreeMap::new();
        // The tool finishes first, but a run with any finished
        // prompt or reflector at all reads only those, so the frame's
        // own prompt-only choice never loses to a tool that merely
        // happened to finish earlier.
        flat.insert(
            "prime.warmup".to_string(),
            combined_tool_node("2026-01-01T00:00:00Z", Some("2026-01-01T00:00:01Z")),
        );
        flat.insert(
            "prime.step".to_string(),
            combined_tool_node("2026-01-01T00:00:00Z", Some("2026-01-01T00:00:05Z")),
        );
        let (label, _extra, _col) =
            finished_prompt_first_and_extra(&rows, &flat, &plan, "2026-01-01T00:00:00Z")
                .expect("a finished prompt");
        assert_eq!(label, "step");
    }

    #[test]
    fn build_combined_signal_breaks_a_largest_tie_by_launch_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("do.yml");
        std::fs::write(
            &path,
            "effects:\n  - name: step\n    type: prompt\n    template: hi\n",
        )
        .unwrap();
        let program = crate::plan::compile(&path, None, &indexmap::IndexMap::new()).unwrap();
        let plan = PlanTree::from_program(&program);
        let now = "2026-01-01T00:00:03Z";
        let anchor = "2026-01-01T00:00:00Z";

        // Three runs, tied at the same total -- the first launched
        // (ra, read first from `inputs`) wins the tie, not whichever
        // `Iterator::max_by_key` would otherwise keep last.
        let make = |doc: &str, model: &mut RunModel, state: &serde_json::Value| {
            crate::render::build_folded(
                doc,
                "cof",
                &plan,
                model,
                Some(state),
                crate::model::ProcessState::Running,
                0.0,
                now,
                &[],
                true,
                &Default::default(),
            )
        };
        let state = finished_prompt_state(124, 0, "2026-01-01T00:00:01Z");
        let flat = crate::model::flatten_state(&state);
        let mut model_a = RunModel::new();
        let rendered_a = make("ra.yml", &mut model_a, &state);
        let mut model_b = RunModel::new();
        let rendered_b = make("rb.yml", &mut model_b, &state);
        let mut model_c = RunModel::new();
        let rendered_c = make("rc.yml", &mut model_c, &state);

        let inputs = vec![
            combined_run_input(
                "ra.yml",
                &rendered_a.header,
                &rendered_a.unfolded_rows,
                &flat,
                &plan,
                &model_a,
            ),
            combined_run_input(
                "rb.yml",
                &rendered_b.header,
                &rendered_b.unfolded_rows,
                &flat,
                &plan,
                &model_b,
            ),
            combined_run_input(
                "rc.yml",
                &rendered_c.header,
                &rendered_c.unfolded_rows,
                &flat,
                &plan,
                &model_c,
            ),
        ];
        let screen = build_combined_signal(&inputs, anchor, 0, 0, now, None);
        assert_eq!(screen.readings.largest_doc, Some("ra.yml".to_string()));
    }
}
