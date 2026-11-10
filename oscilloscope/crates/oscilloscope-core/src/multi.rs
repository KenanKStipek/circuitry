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
use crate::model::{ProcessState, RunModel, StatusKind, flatten_state};
use crate::plan::PlanTree;
use crate::render::Header;
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
    // going on to 1×4 (0→0, no further gain).
    let baseline_cols = max_cols.min(run_count);
    let empty_cells = |cols: usize| -> usize {
        if cols == 0 {
            return usize::MAX;
        }
        let rows = run_count.div_ceil(cols).min(max_rows);
        let capacity = cols * rows;
        capacity.saturating_sub(run_count.min(capacity))
    };
    let mut cols = baseline_cols;
    let mut best_empty = empty_cells(cols);
    while cols > 1 {
        let candidate = cols - 1;
        let candidate_empty = empty_cells(candidate);
        if candidate_empty < best_empty {
            cols = candidate;
            best_empty = candidate_empty;
        } else {
            break;
        }
    }
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
    let Some(now_idx) = items.iter().enumerate().skip(1).find_map(|(i, it)| {
        is_now_marker(it).then_some(i)
    }) else {
        return items.iter().take(max_rows.max(1)).map(TapeTileLine::Item).collect();
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

    let mandatory = 1 + kept_head.len() + usize::from(has_run_end);
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
    let mut magnitude = 1_000u64;
    while magnitude <= need.max(1_000) * 10 {
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
/// stacked (or, for tiles, largest per-tile) column across every
/// run, at or above 1.5× that largest column, growing only. `floor`
/// is the previous frame's own top (`0` for a first computation),
/// so the scale only ever grows across a live run's own redraws.
pub fn shared_signal_axis_top(largest_column: u64, floor: u64) -> u64 {
    let need = (largest_column as f64 * 1.5).ceil() as u64;
    round_top_series_min(floor.max(1_000), need)
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
}

/// Builds one tile's own [`TileRender`] (issue #698): no row
/// selection of its own (`selected_path: None` throughout -- a tile
/// has no interactive cursor; `enter` opens the real, selectable
/// screen instead) and no previews (there is no room for a second
/// line per row at a tile's own width either way).
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
        Screen::Tape => TileScreen::Tape(crate::screens::build_tape(
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
        )),
        Screen::Signal => TileScreen::Signal(crate::screens::build_signal(
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
        )),
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
    TileRender {
        header: rendered.header,
        screen,
    }
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
            items.push(TapeItem::Row(row(&format!("ahead{i}"), StatusKind::Pending)));
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
        assert_eq!(folded.len(), items.len());
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

    #[test]
    fn run_role_index_repeats_past_six_runs() {
        assert_eq!(run_role_index(0), 0);
        assert_eq!(run_role_index(5), 5);
        assert_eq!(run_role_index(6), 0);
        assert_eq!(run_role_index(7), 1);
    }
}
