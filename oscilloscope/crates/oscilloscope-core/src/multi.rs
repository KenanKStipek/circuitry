//! The All screen's own pure logic (design v1 part 5, issue #698):
//! the tile grid (§2.6/§5), folding a tile's own Tape rows to its
//! fixed height, and the shared Signal scale across tiles (Settled
//! point 6) -- all terminal- and IO-independent, so a test builds
//! its own inputs with no real run or terminal involved. `tui.rs`
//! is the only thing that ever turns a [`GridLayout`] into real
//! [`ratatui`] rects, or a [`TapeTileLine`] into drawn cells.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::format::ClockZone;
use crate::model::{NodeMeta, ProcessState, RunModel, StatusKind, flatten_state};
use crate::plan::PlanTree;
use crate::render::{Header, Row, RowKind};
use crate::screens::{Screen, TapeItem};
use crate::store::StoreScreen;

/// A tile's own minimum size (§5: "Tiles at least 56×20").
pub const TILE_MIN_WIDTH: u16 = 56;
pub const TILE_MIN_HEIGHT: u16 = 20;

/// The grid rule (§2.6/§5): "Columns are as many 56-cell tiles as
/// fit the width, rows as many as the runs need, then fewer columns
/// when that leaves fewer empty cells ... A tile is at least 56×20."
/// `body_height` is the area left for tile rows once the header
/// band, status line and key bar are already subtracted (3 rows on
/// every size this design draws All at) -- each tile row itself
/// also carries its own one-row document band above its own panel
/// (§2.6: "triage.yml config-triage.json · ● 2.2s cof"), so a tile
/// row's own real height is a panel of [`TILE_MIN_HEIGHT`] plus that
/// one band row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridLayout {
    pub columns: usize,
    pub rows: usize,
    /// How many real tiles this page shows -- less than
    /// `columns * rows` only when paging (§9.6/Settled point 5):
    /// the last slot then holds a `… n more` tile instead of a real
    /// one.
    pub page_size: usize,
}

impl GridLayout {
    /// How many tile slots the grid actually draws, real tiles and
    /// a trailing `… n more` slot together.
    pub fn slots(&self) -> usize {
        self.columns * self.rows
    }

    pub fn has_more_slot(&self, run_count: usize) -> bool {
        run_count > self.page_size
    }
}

/// One tile row's own real height, band included.
fn tile_row_height() -> u16 {
    TILE_MIN_HEIGHT + 1
}

/// [`GridLayout::columns`]/[`GridLayout::rows`] for `run_count` runs
/// in a grid `width` columns by `body_height` rows tall (the Tape/
/// Signal body only, chrome already subtracted) -- `run_count == 0`
/// still returns a one-tile grid (the caller never actually draws
/// one with nothing running; All is never entered that way, since
/// bare `osi` attaches or shows Select instead).
pub fn tile_grid(run_count: usize, width: u16, body_height: u16) -> GridLayout {
    let run_count = run_count.max(1);
    let max_cols = (width / TILE_MIN_WIDTH).max(1) as usize;
    let max_rows = (body_height / tile_row_height()).max(1) as usize;

    // Start from as many columns as fit (capped at the run count --
    // never more columns than there are runs to show on one page),
    // then only move to fewer columns while that strictly reduces
    // the grid's own empty cells -- stopping the moment it no longer
    // does, so four runs settle on 2×2 (empty cells 2→0) rather than
    // going on to 1×4 (0→0, no further gain). A column count that
    // would need more rows than fit (and so would page) is never a
    // candidate while any column count still lets every run fit on
    // one page -- paging is a last resort, never a side effect of
    // comparing a page that fits against one that would overflow.
    let baseline_cols = max_cols.min(run_count);
    let fits_on_one_page =
        |cols: usize| -> bool { cols > 0 && run_count.div_ceil(cols) <= max_rows };
    let empty_cells_fitting = |cols: usize| -> usize {
        let rows = run_count.div_ceil(cols);
        cols * rows - run_count
    };
    let empty_cells_capped = |cols: usize| -> usize {
        if cols == 0 {
            return usize::MAX;
        }
        let rows = run_count.div_ceil(cols).min(max_rows);
        let capacity = cols * rows;
        capacity.saturating_sub(run_count.min(capacity))
    };
    let cols = if fits_on_one_page(baseline_cols) {
        let mut cols = baseline_cols;
        let mut best_empty = empty_cells_fitting(cols);
        while cols > 1 {
            let candidate = cols - 1;
            if !fits_on_one_page(candidate) {
                break;
            }
            let candidate_empty = empty_cells_fitting(candidate);
            if candidate_empty < best_empty {
                cols = candidate;
                best_empty = candidate_empty;
            } else {
                break;
            }
        }
        cols
    } else {
        // No column count lets every run fit on one page -- this
        // run count pages regardless, so fall back to comparing
        // capped (and therefore equally overflowing) candidates.
        let mut cols = baseline_cols;
        let mut best_empty = empty_cells_capped(cols);
        while cols > 1 {
            let candidate = cols - 1;
            let candidate_empty = empty_cells_capped(candidate);
            if candidate_empty < best_empty {
                cols = candidate;
                best_empty = candidate_empty;
            } else {
                break;
            }
        }
        cols
    };
    let rows = run_count.div_ceil(cols).min(max_rows).max(1);
    let capacity = cols * rows;
    let page_size = if run_count > capacity {
        // §9.6/Settled point 5: paging shows the first page and a
        // `… n more` tile in the grid's own last slot -- one of the
        // grid's own slots, not an extra one beyond it.
        capacity.saturating_sub(1)
    } else {
        run_count
    };
    GridLayout {
        columns: cols,
        rows,
        page_size,
    }
}

/// A folded line of a Tape tile's own list (issue #698, Settled
/// point 5): wraps the owning [`TapeItem`]s this tile actually
/// draws, or one of the two fold markers that stand in for the rows
/// a short tile has no room for -- "the oldest fold into `… n
/// earlier`", "rows ahead ... folding into `… n more`".
#[derive(Debug, Clone, PartialEq)]
pub enum TapeTileLine<'a> {
    Item(&'a TapeItem),
    EarlierFold(usize),
    MoreFold(usize),
    /// The rule between the running block and the rows ahead when
    /// no loop is running to give that rule its own `SlotsRule`
    /// text (from the frames: a tile always draws one rule there,
    /// labelled or not, where a single-run Tape draws none at all
    /// without a running loop).
    RunningSeparator,
}

fn is_ahead_row(item: &TapeItem) -> bool {
    matches!(
        item,
        TapeItem::Row(r) if matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped)
    )
}

fn is_now_marker(item: &TapeItem) -> bool {
    matches!(item, TapeItem::NowBand { .. } | TapeItem::HeadRule)
}

/// Folds a Tape tile's own [`TapeItem`]s down to `max_rows` lines
/// (issue #698, Settled point 5): the Run Started row, the running
/// rows (and the slots rule, when present) and the Run End row are
/// never folded -- only the finished rows above Now (oldest first)
/// and the rows ahead (farthest first) ever give way, each into its
/// own one-line fold marker once any of its own rows no longer fit.
/// Finished rows give way before rows ahead do, so a short tile
/// loses its own history before it loses what's coming next.
pub fn fold_tape_tile(items: &[TapeItem], max_rows: usize) -> Vec<TapeTileLine<'_>> {
    if items.is_empty() {
        return Vec::new();
    }
    let head = &items[0];
    // The Starting state has no second marker and nothing after its
    // own single `HeadRule` at all -- there is nothing left here to
    // fold.
    let Some(now_idx) = items
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(i, it)| is_now_marker(it).then_some(i))
    else {
        return items
            .iter()
            .take(max_rows.max(1))
            .map(TapeTileLine::Item)
            .collect();
    };
    let finished = &items[1..now_idx];
    let rest = &items[now_idx..];
    let has_run_end = matches!(rest.last(), Some(TapeItem::RunEnd { .. }));
    let body = if has_run_end {
        &rest[..rest.len() - 1]
    } else {
        rest
    };
    let ahead_start = body.iter().position(is_ahead_row).unwrap_or(body.len());
    let kept_head = &body[..ahead_start];
    let ahead = &body[ahead_start..];
    let has_slots_rule = kept_head
        .iter()
        .any(|it| matches!(it, TapeItem::SlotsRule { .. }));

    let mandatory = 1 + kept_head.len() + usize::from(!has_slots_rule) + usize::from(has_run_end);
    let budget = max_rows.saturating_sub(mandatory);

    let mut fin_shown = finished.len();
    let mut ahead_shown = ahead.len();
    let lines_for = |fin_shown: usize, ahead_shown: usize| -> usize {
        let fin_lines = fin_shown + usize::from(fin_shown < finished.len());
        let ahead_lines = ahead_shown + usize::from(ahead_shown < ahead.len());
        fin_lines + ahead_lines
    };
    while lines_for(fin_shown, ahead_shown) > budget {
        if fin_shown > 0 {
            fin_shown -= 1;
        } else if ahead_shown > 0 {
            ahead_shown -= 1;
        } else {
            break;
        }
    }

    let mut out = Vec::with_capacity(max_rows);
    out.push(TapeTileLine::Item(head));
    let fin_folded = finished.len() - fin_shown;
    if fin_folded > 0 {
        out.push(TapeTileLine::EarlierFold(fin_folded));
    }
    out.extend(finished[fin_folded..].iter().map(TapeTileLine::Item));
    out.extend(kept_head.iter().map(TapeTileLine::Item));
    if !has_slots_rule {
        out.push(TapeTileLine::RunningSeparator);
    }
    out.extend(ahead[..ahead_shown].iter().map(TapeTileLine::Item));
    let ahead_folded = ahead.len() - ahead_shown;
    if ahead_folded > 0 {
        out.push(TapeTileLine::MoreFold(ahead_folded));
    }
    if has_run_end {
        out.push(TapeTileLine::Item(rest.last().unwrap()));
    }
    out
}

/// §3.5's own doubling series, extended for the shared scale across
/// tiles (issue #698, Settled point 6): the smallest of `{1, 2, 2.5,
/// 4, 5, 8} × 10^n` at or above `need`, growing only and only by
/// whole steps from whatever `floor` already is (never sliding back
/// down, and never skipping a step on the way up).
fn round_top_series_min(floor: u64, need: u64) -> u64 {
    const STEPS: [f64; 6] = [1.0, 2.0, 2.5, 4.0, 5.0, 8.0];
    if need <= floor {
        return floor;
    }
    let mut candidates: Vec<u64> = Vec::new();
    let mut magnitude = 100u64;
    while magnitude <= need.max(100) * 10 {
        for step in STEPS {
            let value = (step * magnitude as f64).round() as u64;
            if value >= floor {
                candidates.push(value);
            }
        }
        magnitude *= 10;
    }
    candidates
        .into_iter()
        .filter(|&v| v >= need)
        .min()
        .unwrap_or(need)
}

/// The Combined Signal's and the Signal tiles' own shared scale
/// (issue #698, Settled point 6): one round top for the largest
/// stacked (or, for tiles, largest single finished effect's own
/// sent or received tokens, [`largest_single_effect_tokens`]) column
/// across every run, at or above 1.5× that largest column, growing
/// only. `floor` is the previous frame's own top (`0` for a first
/// computation), so the scale only ever grows across a live run's
/// own redraws.
pub fn shared_signal_axis_top(largest_column: u64, floor: u64) -> u64 {
    let need = (largest_column as f64 * 1.5).ceil() as u64;
    round_top_series_min(floor, need)
}

/// The largest single finished effect's own sent and received tokens
/// anywhere in one run's own flattened state (issue #698, Settled
/// point 6's own "largest column seen so far"): a Signal bucket can
/// sum several effects that happen to finish within the same 100 ms
/// window (a loop's own concurrent passes, say), which is not what
/// the shared scale's own headroom is over -- this is the same
/// figure the Tape's own `is_largest` marker already tracks, read
/// straight off every node instead of only the run's single largest
/// one.
pub fn largest_single_effect_tokens(flat: &BTreeMap<String, crate::model::NodeMeta>) -> (u64, u64) {
    let mut max_sent = 0u64;
    let mut max_received = 0u64;
    for node in flat.values() {
        let (sent, received) = crate::model::node_tokens(&node.meta);
        max_sent = max_sent.max(sent);
        max_received = max_received.max(received);
    }
    (max_sent, max_received)
}

/// The shared Signal scale's own axis label (issue #698, Settled
/// point 6): one decimal place in `k` from 1,000 up (`5.0k`,
/// `8.0k`), the plain token figure below it (`400 t`) -- a single
/// run's own smaller, never-shared axis keeps its existing plain
/// thousands-separated figure (`format::format_tokens_t`) instead.
pub fn format_shared_axis_label(n: u64) -> String {
    if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{n} t")
    }
}

/// A Signal tile's own finished-prompt label group: a frame's
/// `✓ plan +3 · 4,646 t` is a tool-free run of finished prompts
/// whose own finish times land in the same 100 ms Signal column --
/// as many tokens merge there as the trace's own bucketing already
/// puts in one column, never a separate rule of its own. `label` is
/// the group's own earliest finisher; `extra` is every other one in
/// it (the frame's own `+N`); `tokens` is their sum.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalFinishedLabelGroup {
    pub label: String,
    pub extra: usize,
    pub tokens: u64,
}

/// The tile's own two most recent [`SignalFinishedLabelGroup`]s,
/// oldest first (issue #698, Settled point 5's own Now pane
/// reasoning extended to the chart's own finished labels: a tile
/// has room for two lines under its chart, so only the two most
/// recent groups ever show, dropping any older ones entirely rather
/// than cramming more in). Only finished prompts count -- a
/// finished tool never gets a `✓` mark on the axis either (issue
/// #698's own Signal tile rule), so it never anchors a label group
/// here.
pub fn signal_tile_finished_labels(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    run_start: Option<&str>,
) -> Vec<SignalFinishedLabelGroup> {
    let elapsed_offset = |ts: &str| run_start.and_then(|s| crate::observe::duration_seconds(s, ts));
    let mut finishes: Vec<(usize, &Row)> = Vec::new();
    for row in rows {
        if !matches!(row.kind, RowKind::Prompt | RowKind::Reflector) {
            continue;
        }
        if !matches!(
            row.status.kind,
            StatusKind::Done | StatusKind::FailedHandled
        ) {
            continue;
        }
        let Some(node) = flat.get(&row.path) else {
            continue;
        };
        let Some(ts) = node.completed_at.as_deref() else {
            continue;
        };
        let Some(offset) = elapsed_offset(ts) else {
            continue;
        };
        let col = (offset / 0.1).floor().max(0.0) as usize;
        finishes.push((col, row));
    }
    // `rows` is already plan order; a stable sort on the column
    // alone keeps every tie in that same order, so the group's own
    // first member is always its earliest finisher (ties included).
    finishes.sort_by_key(|(col, _)| *col);
    let mut groups: Vec<(usize, Vec<&Row>)> = Vec::new();
    for (col, row) in finishes {
        match groups.last_mut() {
            Some((last_col, members)) if *last_col == col => members.push(row),
            _ => groups.push((col, vec![row])),
        }
    }
    groups
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(|(_, members)| {
            let tokens: u64 = members
                .iter()
                .map(|r| {
                    flat.get(&r.path)
                        .map(|n| {
                            let (sent, received) = crate::model::node_tokens(&n.meta);
                            sent + received
                        })
                        .unwrap_or(0)
                })
                .sum();
            SignalFinishedLabelGroup {
                label: crate::screens::tape_row_label(members[0], flat, plan),
                extra: members.len() - 1,
                tokens,
            }
        })
        .collect()
}

/// A tile's own pending-loop row drops the `each <name>` clause
/// once the plan already knows the loop's own total (the frames'
/// `drafts            loop · 3 passes`, never `loop · each topic ·
/// 3 passes` -- a single-run Tape keeps the fuller text, which is
/// why this rewrites the already-built string rather than changing
/// the shared row-content code both draw from).
fn simplify_tile_pending_loop_text(items: &mut [TapeItem]) {
    for item in items {
        if let TapeItem::Row(row) = item {
            if row.kind == crate::render::RowKind::Loop && row.status.kind == StatusKind::Pending {
                if let Some((prefix, rest)) = row.what.split_once(" · each ") {
                    if let Some((_, passes)) = rest.rsplit_once(" · ") {
                        if passes.ends_with("passes") {
                            row.what = format!("{prefix} · {passes}");
                        }
                    }
                }
            }
        }
    }
}

/// One row of a Signal tile's own Now pane: `label` and `what` are
/// exactly the Tape's own row text for the same leaf (relative to
/// its own loop, already carrying a queued pass's own suffix),
/// never this screen's own breadcrumb-based `SignalRunningRow`.
#[derive(Debug, Clone, PartialEq)]
pub struct TileNowRow {
    pub label: String,
    pub what: String,
}

/// A tile's own Now pane content, read straight off the same
/// `TapeItem`s a Tape tile already folds: the slots rule's own
/// text, then every running row in list order, then every row
/// ahead (a loop's own queued pass included, already Pending there
/// for as long as it has no dispatched node of its own at all).
pub fn tile_now_pane_rows(
    items: &[TapeItem],
) -> (Option<String>, Vec<TileNowRow>, Vec<TileNowRow>) {
    let mut slots_text = None;
    let mut running = Vec::new();
    let mut ahead = Vec::new();
    for item in items {
        match item {
            TapeItem::SlotsRule {
                busy,
                slots,
                queued,
            } => {
                slots_text = Some(if *queued > 0 {
                    format!("{busy} of {slots} Slots Busy · {queued} Pass Queued")
                } else {
                    format!("{busy} of {slots} Slots Busy")
                });
            }
            TapeItem::Row(row) => match row.status.kind {
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => {
                    running.push(TileNowRow {
                        label: row.label.clone(),
                        what: row.what.clone(),
                    });
                }
                StatusKind::Pending | StatusKind::Skipped => {
                    ahead.push(TileNowRow {
                        label: row.label.clone(),
                        what: row.what.clone(),
                    });
                }
                _ => {}
            },
            _ => {}
        }
    }
    (slots_text, running, ahead)
}

/// Six roles (`run.1`..`run.6`, Settled point 13 of issue #698) --
/// past six runs the set repeats (epic #677's §9 point 5 settles
/// that for the Combined Signal; a tile itself only ever needs one
/// role per run, never a full palette of its own).
pub fn run_role_index(run_position: usize) -> usize {
    run_position % 6
}

/// One tile's own screen, the single one of the four the All
/// screen's own shared `0`-`3` picked for every tile at once
/// (issue #698, §2.6: "`0`-`3` switch the screen of every tile at
/// once").
pub enum TileScreen {
    Tape(crate::screens::TapeScreen),
    Signal(crate::screens::SignalScreen),
    Flow(crate::screens::FlowScreen),
    Store(StoreScreen),
}

/// One tile's own render (issue #698): the same [`Header`]/screen
/// pair a single-run attach builds, from the same [`RunModel`]/
/// [`PlanTree`]/state a tile's own run would otherwise be attached
/// to directly -- shared by `tui.rs`'s own live All loop and its
/// frame goldens, so the two can never drift apart.
pub struct TileRender {
    pub header: Header,
    pub screen: TileScreen,
    /// The newest finished (or handled-failed) row in this one run
    /// alone, whatever tile screen is actually showing (issue #698's
    /// own status line, §3.2: "the newest finished step across
    /// every run") -- independent of `screen_kind`, so a page of
    /// Flow or Store tiles still has this to feed
    /// [`newest_finished_across_runs`].
    pub last_finished: Option<crate::screens::SignalLastFinished>,
    /// This run's own largest Signal bucket sum (sent, then
    /// received), for the Signal tiles' shared scale -- `0` for
    /// every other screen kind, which never reads either field.
    pub largest_sent: u64,
    pub largest_received: u64,
}

/// Builds one tile's own [`TileRender`] (issue #698): no row
/// selection of its own (`selected_path: None` throughout -- a tile
/// has no interactive cursor; `enter` opens the real, selectable
/// screen instead) and no previews (there is no room for a second
/// line per row at a tile's own width either way).
#[allow(clippy::too_many_arguments)]
pub fn build_tile(
    document_label: &str,
    engine_label: &str,
    plan: &PlanTree,
    model: &mut RunModel,
    state: Option<&Value>,
    screen_kind: Screen,
    zone: &dyn ClockZone,
    now: &str,
) -> TileRender {
    let rendered = crate::render::build_folded(
        document_label,
        engine_label,
        plan,
        model,
        state,
        ProcessState::Running,
        0.0,
        now,
        &[],
        true,
        &Default::default(),
    );
    let empty = Value::Null;
    let flat = flatten_state(state.unwrap_or(&empty));
    let screen = match screen_kind {
        Screen::Tape => {
            let mut tape = crate::screens::build_tape(
                &rendered.rows,
                &rendered.unfolded_rows,
                &rendered.header,
                &flat,
                plan,
                model,
                state,
                None,
                false,
                now,
            );
            simplify_tile_pending_loop_text(&mut tape.items);
            TileScreen::Tape(tape)
        }
        Screen::Signal => {
            let mut signal = crate::screens::build_signal(
                &rendered.rows,
                &rendered.unfolded_rows,
                &rendered.header,
                &flat,
                plan,
                model,
                state,
                100,
                None,
                None,
                now,
            );
            signal.tile_finished_labels = signal_tile_finished_labels(
                &rendered.rows,
                &flat,
                plan,
                rendered.header.run_start_ts.as_deref(),
            );
            // The tile's own Now pane borrows the Tape's row text
            // verbatim (relative names, queued-pass suffixes) --
            // built the same way a Tape tile's own screen is, then
            // read back out of its `items` rather than re-deriving
            // any of that here.
            let mut tape_for_now_pane = crate::screens::build_tape(
                &rendered.rows,
                &rendered.unfolded_rows,
                &rendered.header,
                &flat,
                plan,
                model,
                state,
                None,
                false,
                now,
            );
            simplify_tile_pending_loop_text(&mut tape_for_now_pane.items);
            let (slots_text, running, ahead) = tile_now_pane_rows(&tape_for_now_pane.items);
            signal.tile_now_slots_text = slots_text;
            signal.tile_now_running = running;
            signal.tile_now_ahead = ahead;
            TileScreen::Signal(signal)
        }
        Screen::Flow => {
            let root_path = rendered.unfolded_rows.first().map(|r| r.path.as_str());
            let selected = crate::screens::default_follow_path(&rendered.unfolded_rows)
                .map(str::to_string)
                .or_else(|| root_path.map(str::to_string));
            TileScreen::Flow(crate::screens::build_flow(
                &rendered.unfolded_rows,
                &rendered.rows,
                &flat,
                plan,
                model,
                &rendered.header,
                state,
                &[],
                selected.as_deref(),
                zone,
                rendered.header.run_end_ts.as_deref(),
                now,
            ))
        }
        Screen::Store => {
            let kinds: BTreeMap<String, crate::render::RowKind> = rendered
                .unfolded_rows
                .iter()
                .map(|r| (r.path.clone(), r.kind))
                .collect();
            TileScreen::Store(crate::store::build_store(
                &rendered.unfolded_rows,
                &rendered.header,
                &flat,
                &kinds,
                plan,
                model,
                state,
                "prime",
                now,
                None,
            ))
        }
    };
    let last_finished = crate::screens::last_finished_row(
        &rendered.rows,
        &flat,
        plan,
        rendered.header.run_start_ts.as_deref(),
    );
    // The shared scale's own "largest column seen so far" (Settled
    // point 6) is the trace's own bucket sums -- every token that
    // lands in the same 100 ms column together, not any one effect
    // alone -- so several prompts finishing in the same column (a
    // loop's own concurrent passes) raise it the same way they
    // raise the bar the chart itself draws there.
    let (largest_sent, largest_received) = match &screen {
        TileScreen::Signal(s) => (
            s.buckets.iter().map(|b| b.sent).max().unwrap_or(0),
            s.buckets.iter().map(|b| b.received).max().unwrap_or(0),
        ),
        _ => largest_single_effect_tokens(&flat),
    };
    TileRender {
        header: rendered.header,
        screen,
        last_finished,
        largest_sent,
        largest_received,
    }
}

/// The newest finished (or handled-failed) row across every run
/// (issue #698, §3.2: "the status line shows the newest finished
/// step across every run"), ties broken by run order (`entries`'
/// own order, which the caller already keeps in launch order) then
/// by plan order (already resolved inside each run's own
/// [`crate::screens::last_finished_row`], whose `max_by` keeps the
/// later of two exactly-tied rows in plan-walk order).
pub fn newest_finished_across_runs(
    entries: &[(String, Option<crate::screens::SignalLastFinished>)],
) -> Option<(String, crate::screens::SignalLastFinished)> {
    let mut best: Option<(i64, usize)> = None;
    for (i, (_, lf)) in entries.iter().enumerate() {
        let Some(lf) = lf else { continue };
        let Some(ts) = lf.ts.as_deref() else { continue };
        let Some(parsed) = crate::diff::parse_instant(ts) else {
            continue;
        };
        let numeric = parsed.timestamp_millis();
        let replace = match best {
            Some((best_ts, _)) => numeric > best_ts,
            None => true,
        };
        if replace {
            best = Some((numeric, i));
        }
    }
    best.map(|(_, i)| (entries[i].0.clone(), entries[i].1.clone().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RowStatus;
    use crate::render::RowKind;
    use crate::screens::TapeRow;

    #[test]
    fn tile_grid_six_runs_at_180x48_is_3x2() {
        let grid = tile_grid(6, 180, 45);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 2);
        assert_eq!(grid.page_size, 6);
    }

    #[test]
    fn tile_grid_four_runs_rebalances_to_2x2() {
        let grid = tile_grid(4, 180, 45);
        assert_eq!(grid.columns, 2);
        assert_eq!(grid.rows, 2);
    }

    #[test]
    fn tile_grid_three_runs_is_one_row() {
        let grid = tile_grid(3, 180, 45);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 1);
    }

    #[test]
    fn tile_grid_at_180x24_fits_three_in_one_row() {
        // §5: "three fit 180×24" -- one tile row (21 rows) fits in a
        // 21-row body.
        let grid = tile_grid(3, 180, 21);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 1);
    }

    #[test]
    fn tile_grid_at_120x30_holds_two_tiles_and_pages_the_rest() {
        // §5: "120×30 holds two tiles and pages the rest" -- a
        // 27-row body (30 - 3 chrome rows) fits one 21-row tile row,
        // two 60-wide columns fit in 120.
        let grid = tile_grid(6, 120, 27);
        assert_eq!(grid.columns, 2);
        assert_eq!(grid.rows, 1);
        assert!(grid.has_more_slot(6));
        assert_eq!(grid.page_size, 1);
    }

    #[test]
    fn tile_grid_one_run_is_a_single_tile() {
        let grid = tile_grid(1, 180, 45);
        assert_eq!(grid.columns, 1);
        assert_eq!(grid.rows, 1);
        assert!(!grid.has_more_slot(1));
    }

    #[test]
    fn tile_grid_eight_runs_at_180x48() {
        // max_cols = 3, max_rows = 2 -> capacity 6, so 8 runs page.
        let grid = tile_grid(8, 180, 45);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 2);
        assert!(grid.has_more_slot(8));
        assert_eq!(grid.page_size, 5);
    }

    #[test]
    fn tile_grid_two_runs_is_2x1() {
        let grid = tile_grid(2, 180, 45);
        assert_eq!(grid.columns, 2);
        assert_eq!(grid.rows, 1);
    }

    #[test]
    fn tile_grid_five_runs_at_180x48_fits_3x2_without_paging() {
        // 3x2 has room for all 5 runs with one empty cell -- never
        // paged in favour of a 2x2 grid that would have to drop a
        // run into "... n more" purely because its own capped-rows
        // bookkeeping read as "0 empty cells".
        let grid = tile_grid(5, 180, 45);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 2);
        assert_eq!(grid.page_size, 5);
        assert!(!grid.has_more_slot(5));
    }

    #[test]
    fn tile_grid_five_runs_at_240_wide_fits_without_paging() {
        let grid = tile_grid(5, 240, 45);
        assert!(!grid.has_more_slot(5));
        assert_eq!(grid.page_size, 5);
    }

    #[test]
    fn tile_grid_seven_runs_at_180x48_pages() {
        // max_cols 3, max_rows 2 -> capacity 6 < 7 runs: no column
        // count fits every run on one page, so this one still pages.
        let grid = tile_grid(7, 180, 45);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 2);
        assert!(grid.has_more_slot(7));
        assert_eq!(grid.page_size, 5);
    }

    #[test]
    fn tile_grid_nine_runs_at_180x48_pages() {
        let grid = tile_grid(9, 180, 45);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows, 2);
        assert!(grid.has_more_slot(9));
        assert_eq!(grid.page_size, 5);
    }

    fn row(path: &str, status: StatusKind) -> TapeRow {
        TapeRow {
            path: path.to_string(),
            label: path.to_string(),
            kind: RowKind::Tool,
            status: RowStatus {
                kind: status,
                skip_reason: None,
                retrying: false,
            },
            ts: None,
            what: String::new(),
            what_is_agent_text: false,
            preview: None,
            bold: false,
            tokens: Default::default(),
        }
    }

    fn sample_items(finished: usize, running: usize, ahead: usize) -> Vec<TapeItem> {
        let mut items = vec![TapeItem::RunStarted {
            ts: None,
            document: "d.yml".to_string(),
            engine: "cof 0.1.0".to_string(),
        }];
        for i in 0..finished {
            items.push(TapeItem::Row(row(&format!("fin{i}"), StatusKind::Done)));
        }
        items.push(TapeItem::NowBand {
            ts: None,
            elapsed_s: 1.0,
        });
        for i in 0..running {
            items.push(TapeItem::Row(row(&format!("run{i}"), StatusKind::Running)));
        }
        for i in 0..ahead {
            items.push(TapeItem::Row(row(
                &format!("ahead{i}"),
                StatusKind::Pending,
            )));
        }
        items.push(TapeItem::RunEnd {
            usual_total_tokens: None,
            usual_wall_s: None,
            eta_ts: None,
        });
        items
    }

    #[test]
    fn fold_tape_tile_keeps_everything_when_it_fits() {
        let items = sample_items(2, 1, 2);
        let folded = fold_tape_tile(&items, 20);
        // One more line than `items` itself: no loop runs here, so
        // the running-block separator (never a real `TapeItem`) is
        // the one synthetic line this ever inserts unprompted.
        assert_eq!(folded.len(), items.len() + 1);
    }

    #[test]
    fn fold_tape_tile_folds_finished_rows_before_ahead_rows() {
        let items = sample_items(5, 1, 3);
        // mandatory = RunStarted + Now + 1 running + RunEnd = 4;
        // budget for 10 rows total = 6, finished(5)+ahead(3) = 8 > 6
        // -- finished gives way first.
        let folded = fold_tape_tile(&items, 10);
        assert_eq!(folded.len(), 10);
        let earlier = folded
            .iter()
            .find_map(|l| match l {
                TapeTileLine::EarlierFold(n) => Some(*n),
                _ => None,
            })
            .unwrap();
        assert!(earlier > 0, "finished rows should fold first");
        assert!(
            !folded
                .iter()
                .any(|l| matches!(l, TapeTileLine::MoreFold(_))),
            "ahead rows all still fit"
        );
    }

    #[test]
    fn fold_tape_tile_folds_ahead_rows_once_finished_is_exhausted() {
        let items = sample_items(1, 1, 8);
        let folded = fold_tape_tile(&items, 8);
        let more = folded.iter().find_map(|l| match l {
            TapeTileLine::MoreFold(n) => Some(*n),
            _ => None,
        });
        assert!(more.is_some());
    }

    #[test]
    fn fold_tape_tile_never_folds_running_rows_or_the_anchors() {
        let items = sample_items(3, 2, 3);
        let folded = fold_tape_tile(&items, 6);
        let running_shown = folded
            .iter()
            .filter(|l| matches!(l, TapeTileLine::Item(TapeItem::Row(r)) if r.status.kind == StatusKind::Running))
            .count();
        assert_eq!(running_shown, 2);
        assert!(
            folded
                .iter()
                .any(|l| matches!(l, TapeTileLine::Item(TapeItem::RunStarted { .. })))
        );
        assert!(
            folded
                .iter()
                .any(|l| matches!(l, TapeTileLine::Item(TapeItem::RunEnd { .. })))
        );
    }

    #[test]
    fn shared_signal_axis_top_matches_the_frames_5_0k_and_8_0k() {
        // Issue #698's own data notes: 1.5 x 3,160 -> 4,740 -> 5.0k;
        // 1.5 x 4,912 -> 7,368 -> 8.0k.
        assert_eq!(shared_signal_axis_top(3_160, 0), 5_000);
        assert_eq!(shared_signal_axis_top(4_912, 0), 8_000);
    }

    #[test]
    fn shared_signal_axis_top_never_shrinks() {
        let top = shared_signal_axis_top(4_912, 0);
        assert_eq!(shared_signal_axis_top(100, top), top);
    }

    #[test]
    fn shared_signal_axis_top_grows_by_whole_steps() {
        let first = shared_signal_axis_top(1_000, 0);
        let grown = shared_signal_axis_top(10_000, first);
        assert!(grown > first);
    }

    fn last_finished(label: &str, ts: &str) -> crate::screens::SignalLastFinished {
        crate::screens::SignalLastFinished {
            label: label.to_string(),
            kind: RowKind::Tool,
            status: StatusKind::Done,
            tokens: None,
            ts: Some(ts.to_string()),
        }
    }

    #[test]
    fn newest_finished_across_runs_picks_the_latest_ts() {
        let entries = vec![
            (
                "a.yml".to_string(),
                Some(last_finished("x", "2026-01-01T00:00:01Z")),
            ),
            (
                "b.yml".to_string(),
                Some(last_finished("y", "2026-01-01T00:00:02Z")),
            ),
        ];
        let (doc, lf) = newest_finished_across_runs(&entries).unwrap();
        assert_eq!(doc, "b.yml");
        assert_eq!(lf.label, "y");
    }

    #[test]
    fn newest_finished_across_runs_breaks_a_tie_by_run_order() {
        let entries = vec![
            (
                "a.yml".to_string(),
                Some(last_finished("x", "2026-01-01T00:00:01Z")),
            ),
            (
                "b.yml".to_string(),
                Some(last_finished("y", "2026-01-01T00:00:01Z")),
            ),
        ];
        let (doc, _) = newest_finished_across_runs(&entries).unwrap();
        assert_eq!(doc, "a.yml", "the earlier run order wins an exact tie");
    }

    #[test]
    fn newest_finished_across_runs_skips_runs_with_nothing_finished_yet() {
        let entries = vec![
            ("a.yml".to_string(), None),
            (
                "b.yml".to_string(),
                Some(last_finished("y", "2026-01-01T00:00:01Z")),
            ),
        ];
        let (doc, _) = newest_finished_across_runs(&entries).unwrap();
        assert_eq!(doc, "b.yml");
    }

    #[test]
    fn shared_signal_axis_top_reaches_below_1_000_for_a_small_column() {
        // Issue #698's own data note: the received top (`400 t`)
        // is below 1,000 -- the label format starts using `k` at
        // 1,000, but the value itself is never floored there.
        assert_eq!(shared_signal_axis_top(206, 0), 400);
    }

    #[test]
    fn format_shared_axis_label_matches_the_frames() {
        assert_eq!(format_shared_axis_label(5_000), "5.0k");
        assert_eq!(format_shared_axis_label(8_000), "8.0k");
        assert_eq!(format_shared_axis_label(2_500), "2.5k");
        assert_eq!(format_shared_axis_label(400), "400 t");
    }

    #[test]
    fn run_role_index_repeats_past_six_runs() {
        assert_eq!(run_role_index(0), 0);
        assert_eq!(run_role_index(5), 5);
        assert_eq!(run_role_index(6), 0);
        assert_eq!(run_role_index(7), 1);
    }
}
