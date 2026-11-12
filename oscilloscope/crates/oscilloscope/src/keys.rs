//! `App`: the TUI's own navigation/dialog state and key handling
//! (DESIGN.md §6.3's key table, issue #434). Pure and
//! terminal-independent beyond `crossterm::event::KeyEvent` itself —
//! every test here builds its own `Row`s and feeds in key events
//! directly, with no real terminal involved.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oscilloscope_core::render::{FullValueField, Row, StateRow};
use oscilloscope_core::screens::Screen;

/// DESIGN.md §6.3's `tab` cycle, issue #527: `Tree` → `Details` →
/// `State` → `Tree`. The treemap and token bar have no pane of their
/// own to focus — neither scrolls or takes a selection independent of
/// the orchestration pane's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Details,
    State,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    None,
    /// `c`, or `q` while the engine is still running (§6.3: "`q` ...
    /// asks if a run is going") — `quit_after` tells `y` which of the
    /// two this confirm is actually for, since both land here.
    ConfirmCancel {
        quit_after: bool,
    },
    Help,
    FullValue(FullValueField),
    /// `/`'s own text entry, before it becomes the applied
    /// `App::filter`.
    FilterInput(String),
}

/// What the main loop should do after a key, beyond the `App` state
/// `handle_key` already updated in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing this key does needs the main loop's own attention.
    None,
    /// Forward a cancelling signal to the engine (`c`, or a confirmed
    /// `q` while it's running) — the same thing Ctrl-C already does,
    /// so the main loop reuses its own `forward`/`signal_count` path.
    Cancel,
    /// Leave the TUI now: `q` with nothing running, or `q` confirmed
    /// to cancel while `osi watch`'s own `quit_after` has nothing to
    /// wait on cancelling at all (watch owns no engine to cancel —
    /// `q` there always just detaches, §6.3).
    Quit,
    /// Ctrl-C (review finding K3), checked before any dialog and
    /// never merely dismissing one: the main loop forwards SIGINT at
    /// once with no confirm while something's running (a second
    /// Ctrl-C starts the same 10s kill deadline a real repeated
    /// SIGINT would), quits immediately with nothing running, and in
    /// `osi watch` always just detaches — the same three cases a real
    /// forwarded SIGINT already covers in plain mode.
    CtrlC,
    /// `t` (issue #535): cycle to the next colour theme, wrapping.
    CycleTheme,
    /// `esc` on a run opened from the Select screen (issue #698,
    /// §2.5: "`esc` from the run comes back here") -- never fires
    /// unless `App::opened_from_select` is set, so a plain `osi
    /// <doc>`/`osi watch` run's own `esc` (already unbound at this
    /// level) is unaffected.
    BackToSelect,
}

/// The TUI's whole navigation/dialog state (DESIGN.md §6.3). `running`
/// is the caller's own signal of whether the engine osi started (or,
/// for `osi watch`, the watched run) is still going — it decides
/// whether `q` asks first at all.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub selected_path: Option<String>,
    /// The state pane's own selection (DESIGN.md §6.3, issue #527):
    /// deliberately independent of `selected_path` — "selecting an
    /// entry there does not move the orchestration selection".
    pub state_selected_path: Option<String>,
    pub collapsed: std::collections::HashSet<String>,
    /// The state pane's own collapsed set (issue #535) — deliberately
    /// independent of `collapsed` (the orchestration pane's own),
    /// same as `state_selected_path` is independent of `selected_path`.
    pub state_collapsed: std::collections::HashSet<String>,
    pub pane: Pane,
    pub follow: bool,
    pub errors_only: bool,
    pub filter: Option<String>,
    pub dialog: Dialog,
    /// `osi watch`'s own flavour of quitting (§6.3): `q` never needs a
    /// confirm there, since watch owns no engine of its own to cancel
    /// — it only ever detaches.
    pub is_watch: bool,
    /// Set when this run was attached from the Select screen (issue
    /// #698): `esc` then returns there (`Action::BackToSelect`)
    /// instead of doing nothing, the one difference from a plain
    /// `osi watch`'s own key handling.
    pub opened_from_select: bool,
    /// Set when `q` was confirmed while the engine was still running
    /// (§6.3): the main loop sends the same cancelling signal `c`
    /// would, same as always, but also remembers to leave on its own
    /// once the run actually ends, rather than making the user press
    /// `q` a second time after it does.
    pub quit_when_finished: bool,
    /// The top-of-viewport index into the orchestration pane's own
    /// visible rows, and the state pane's own scroll position, and
    /// the details pane's own top line (issue #527's own scrolling
    /// acceptance criteria). `tui::draw` is the only thing that ever
    /// mutates these — it both clamps them to whatever it just drew
    /// and keeps the current selection in view, which needs the real
    /// rendered height no key handler here ever has.
    pub tree_scroll: usize,
    pub state_scroll: usize,
    pub details_scroll: usize,
    /// The interior height `tui::draw` last rendered each of these
    /// three panes into — `PgUp`/`PgDn`'s own page size, one frame
    /// stale at worst (a resize is also what invalidates it, and a
    /// resize always triggers a redraw before the next key anyway).
    pub tree_viewport: usize,
    pub state_viewport: usize,
    pub details_viewport: usize,
    /// Folding on/off (DESIGN.md, issue #613): on by default. `z`
    /// toggles it to show the full, unfolded tree.
    pub fold_enabled: bool,
    /// Quick-steps groups (keyed by their own representative path,
    /// `oscilloscope_core::fold`'s first-step convention) the user
    /// asked ⏎ to expand in place -- the existing collapse key (`←`)
    /// removes a path from here, folding that group again.
    pub expanded_quick_steps: std::collections::BTreeSet<String>,
    /// Which screen is on display (issue #626, #632, #650): `0`-`3`
    /// all switch here now. Selection, follow, folding, filters and
    /// collapsed rows
    /// all live outside this field, so switching screens never
    /// touches them.
    pub screen: Screen,
    /// Which of the two independent selections (`selected_path`,
    /// `state_selected_path`) the details pane shows (#610): whichever
    /// one last actually changed, not whatever `pane` happens to be
    /// right now. `Tab`'s own cycle (`Tree` → `Details` → `State` →
    /// `Tree`) passes through `Details` with neither of the other two
    /// focused, so a selection made in the state pane must still be
    /// what `Details` (and its own scrolling, which only `Pane::
    /// Details` focus allows) shows once the user tabs there --
    /// `pane == Pane::State` is only true while that pane itself has
    /// focus, never while scrolling its own selection's details.
    pub details_source: Pane,
    /// The rail's own horizontal scroll (issue #691, DESIGN.md
    /// §2.3's follow rule): a character offset from the run's own
    /// start, clamped to the rail's real width only at draw time
    /// (`tui::draw_flow`), the only place that knows it. `←→` move
    /// it and turn `follow` off; `tui::draw_flow` turns `follow` back
    /// on once a manual scroll lands Now back in view, the same way
    /// the Tape's own selection rule already does for a row.
    pub flow_scroll: usize,
    /// Set by the `←→` Flow key handlers alongside turning `follow`
    /// off, consumed by `tui::flow_resolve_scroll` on the very next
    /// draw: that draw may land `follow` back on if it finds Now back
    /// in view, but only that one -- a later redraw with no new `←→`
    /// press (a clock tick, `↑↓` or `f` turning follow off again)
    /// must never flip it back on just because Now already happens to
    /// sit in the current view.
    pub flow_manual_scroll: bool,
    /// The rail's own unfolded, unfiltered rows (`render::RenderState::
    /// unfolded_rows`), refreshed on every rebuild: the Flow screen's
    /// own ↑↓/`+`/`-` keys read this, never `handle_key`'s own `rows`
    /// (the Tree pane's folded, filter-applied list) -- a pass folding
    /// absorbs into a breadcrumb, or whose body is a single nested
    /// container with no `RowKind::Pass` row of its own, is still on
    /// the rail and must still be reachable.
    pub flow_rows: Vec<Row>,
    /// Issue #704 rule 3: the selected lane's own stations in plan
    /// order (`rail::lane_stations`'s own output), refreshed on
    /// every rebuild -- `←→` steps through this list with follow off
    /// or the run ended; `tui::draw_flow` is the only thing that
    /// ever rebuilds it, the same lifecycle `flow_rows` already has.
    pub flow_lane_stations: Vec<String>,
    /// Issue #704 rule 3: what `←` steps out to when the selection is
    /// already at the selected lane's own first station (`rail::
    /// step_out_of_lane`'s own output, `tui::draw_flow`'s own
    /// lifecycle) -- `None` with no enclosing fork at all (already
    /// at the rail's own top level).
    pub flow_step_out_before: Option<String>,
    /// `flow_step_out_before`'s own `→` counterpart, for the lane's
    /// own last station.
    pub flow_step_out_after: Option<String>,
    /// Per-fork lane-open overrides (issue #691's `+`/`-`), keyed by
    /// the fork's own path: `0` folds it into its own band, the
    /// fork's own lane count fully opens it, anything in between is
    /// only reachable on a loop four lanes wide or more (`+` opens
    /// three at a time). Absent means the default rule applies
    /// (folded below 100 columns, or with four lanes or more).
    pub flow_lanes_open: std::collections::BTreeMap<String, usize>,
    /// The Signal screen's own scrub cursor (issue #688, §4: `←→`),
    /// a column index from the trace's own start -- `None` means
    /// following Now, the default; any scrub turns follow off, `n`
    /// restores it. `tui::draw` is the only thing that turns a
    /// column into an actual time or screen position.
    pub signal_cursor_col: Option<usize>,
    /// The Signal screen's own zoom (§3.5/§4: `+`/`-`, 50-1,000 ms
    /// per column, 100 the frame's own default).
    pub signal_column_ms: u64,
    /// The latest redraw's own Now column, cached the same way
    /// `tree_viewport` is, so `signal_scrub`'s own clamp (never past
    /// Now) has something to read.
    pub signal_now_col: Option<usize>,
    /// The latest redraw's own resolved cursor column (`SignalScreen::
    /// cursor_col`, issue #704 rule 2: `signal_cursor_col.or(selection_
    /// col).or(now_col)`) -- wherever the trace actually drew its own
    /// cursor, whether from a live scrub, the shared selection's own
    /// column, or Now with neither. `signal_scrub`'s own first press
    /// seeds from this, never from `signal_now_col` directly, or it
    /// would jump the cursor to Now instead of moving it one column
    /// from wherever it is already drawn.
    pub signal_drawn_col: Option<usize>,
    /// The latest redraw's own trace window start (#734): `signal_drawn_col` can be a column the window no
    /// longer shows at all (a stale selection from long before the
    /// window last scrolled) -- `signal_scrub`'s own first press
    /// seeds from `signal_drawn_col` only when it is still at or
    /// past this, falling back to Now otherwise, never jumping the
    /// cursor back to a column that is off screen.
    pub signal_window_start: Option<usize>,
    /// Whether the latest redraw's own Signal screen has a step
    /// under its cursor at all, cached the same way `signal_now_col`
    /// is -- `enter` on Signal only opens one when this is true, so
    /// it can never toggle `tape_fullscreen_step` on with nothing
    /// for the full-screen view to show yet, only to spring open
    /// once a step finally appears.
    pub signal_has_step: bool,
    /// The Flow screen's own analogue of `signal_has_step`: whether
    /// the latest redraw's own rail has anything selected at all
    /// (a pass, a plain station, or a decision) -- `enter` on Flow
    /// only toggles the fullscreen pane while this is true, for the
    /// same reason.
    pub flow_has_selection: bool,
    /// The Tape's own previews toggle (issue #678, §4's `p`): on by
    /// default at 120 columns and up (§9's settled point 4).
    pub tape_previews_on: bool,
    /// The Tape's own token column toggle (§4's `t`): fixed-scale
    /// bars by default, the deviation-from-usual view once set.
    pub tape_tokens_view: bool,
    /// ⏎ on the Tape (§4): the selected row's own step, full-screen.
    pub tape_fullscreen_step: bool,
    /// `m` (issue #704 rule 4): the selected step's own Meta section
    /// unfolds -- folded by default, on every screen, not per-screen
    /// state (there is only one selection to unfold Meta for).
    pub step_meta_unfolded: bool,
    /// Issue #704 rule 1: set once, the instant the run ends --
    /// guards [`Self::tape_select_run_end_once`] so the jump to the
    /// run's own default row only ever happens that one time, never
    /// again overriding wherever the user has since moved to.
    pub tape_run_end_fired: bool,
    /// Issue #704 rule 1: the exact path `tape_select_run_end_once`
    /// moved the selection to the instant the run ended -- the
    /// detail pane shows the run summary for as long as `selected_
    /// path` is still this (drawing-time comparison, `tui::draw_tape_
    /// detail`'s own `ended_row_selected`), never a separate sticky
    /// flag: the selection can move away through any screen or key
    /// (the Signal, the Flow, `[`/`]` in the full-screen step, not
    /// only the Tape's own `move_tree_selection`/`jump`), and summary
    /// mode must end the moment it does, from every one of them.
    /// Moving back onto this exact row re-enters summary mode just
    /// as readily, the same way any other selection shows its own
    /// details whenever the cursor lands back on it.
    pub tape_run_end_target: Option<String>,
    /// Issue #704 rule 4's own "enter on a pass opens it": the
    /// latest full-screen step draw's own Loop section, one `(line
    /// index, pass path)` pair per pass (`tui::loop_pass_line_
    /// paths`'s own output) -- empty whenever the open step's own
    /// detail isn't a loop's at all. One frame stale at worst, the
    /// same lifecycle `flow_lane_stations` already has.
    pub loop_pass_line_paths: Vec<(usize, String)>,
    /// Issue #710's window rule: the list's own top, in the signed
    /// coordinate `tui::tape_list_window_start` returns (negative or
    /// past the content's own end is exactly the window's own blank
    /// padding). `None` until the Tape has drawn its list at least
    /// once -- with follow off and nothing to resume from yet, the
    /// window still centres the Now area, the same as follow on.
    /// `tui::draw_tape_list` is the only thing that ever sets this,
    /// right after computing it, the same lifecycle `flow_scroll`
    /// already has.
    pub tape_list_start: Option<isize>,
}

impl App {
    pub fn new(is_watch: bool) -> Self {
        App {
            selected_path: None,
            state_selected_path: None,
            collapsed: std::collections::HashSet::new(),
            state_collapsed: std::collections::HashSet::new(),
            pane: Pane::Tree,
            // Issue #527: "follow is on when the TUI starts".
            follow: true,
            errors_only: false,
            filter: None,
            dialog: Dialog::None,
            is_watch,
            opened_from_select: false,
            quit_when_finished: false,
            tree_scroll: 0,
            state_scroll: 0,
            details_scroll: 0,
            tree_viewport: 0,
            state_viewport: 0,
            details_viewport: 0,
            fold_enabled: true,
            expanded_quick_steps: std::collections::BTreeSet::new(),
            details_source: Pane::Tree,
            screen: Screen::default(),
            flow_scroll: 0,
            flow_manual_scroll: false,
            flow_rows: Vec::new(),
            flow_lane_stations: Vec::new(),
            flow_step_out_before: None,
            flow_step_out_after: None,
            flow_lanes_open: std::collections::BTreeMap::new(),
            signal_cursor_col: None,
            signal_drawn_col: None,
            signal_window_start: None,
            signal_column_ms: 100,
            signal_now_col: None,
            signal_has_step: false,
            flow_has_selection: false,
            tape_previews_on: true,
            tape_tokens_view: false,
            tape_fullscreen_step: false,
            tape_run_end_fired: false,
            tape_run_end_target: None,
            loop_pass_line_paths: Vec::new(),
            step_meta_unfolded: false,
            tape_list_start: None,
        }
    }

    /// Issue #704 rule 1: the instant a run ends, the Tape's own
    /// selection moves to `default_target` (the run's own handled
    /// failure, when it has one, else whatever row the caller
    /// otherwise considers worth landing on) and the detail pane
    /// starts showing the run summary rather than that row's own
    /// detail -- exactly once, since a later manual move away from
    /// summary mode must never be pulled back (`running` stays
    /// `false` forever after, but `tape_run_end_fired` already stops
    /// this from firing again). `tape_run_end_target` records exactly
    /// which path that was, so summary mode can later be read off
    /// the live selection instead of a separate flag.
    pub fn tape_select_run_end_once(&mut self, running: bool, default_target: Option<String>) {
        if !running && !self.tape_run_end_fired {
            let target = default_target.or_else(|| self.selected_path.clone());
            if let Some(target) = target.clone() {
                self.select_tree_path_keeping_details(Some(target));
            }
            self.tape_run_end_fired = true;
            self.tape_run_end_target = target;
        }
    }

    /// §3.5/§4's own zoom levels, 50 ms to 1 s per column -- `+`
    /// steps toward the front (finer, more columns), `-` toward the
    /// back.
    const SIGNAL_ZOOM_LEVELS_MS: [u64; 5] = [50, 100, 200, 500, 1000];

    pub fn signal_zoom_in(&mut self) {
        let idx = Self::SIGNAL_ZOOM_LEVELS_MS
            .iter()
            .position(|&v| v == self.signal_column_ms)
            .unwrap_or(1);
        if idx > 0 {
            self.signal_column_ms = Self::SIGNAL_ZOOM_LEVELS_MS[idx - 1];
            // A column's own width just changed, so a scrubbed
            // cursor's own column index no longer points at the
            // same moment it did -- back to following Now, the same
            // as `n`, rather than silently pointing somewhere else.
            self.signal_cursor_col = None;
        }
    }

    pub fn signal_zoom_out(&mut self) {
        let idx = Self::SIGNAL_ZOOM_LEVELS_MS
            .iter()
            .position(|&v| v == self.signal_column_ms)
            .unwrap_or(1);
        if idx + 1 < Self::SIGNAL_ZOOM_LEVELS_MS.len() {
            self.signal_column_ms = Self::SIGNAL_ZOOM_LEVELS_MS[idx + 1];
            self.signal_cursor_col = None;
        }
    }

    /// `←→` (§4): moves the scrub cursor one column at a time, turning
    /// follow off -- the first press off Now seeds from `signal_
    /// drawn_col` (wherever the trace actually drew its own cursor
    /// last, the shared selection's own column included, issue #704
    /// rule 2), never from Now directly, or the first press would
    /// jump the cursor there instead of moving it one column from
    /// wherever it already is. #734: that column
    /// is only honoured when the latest draw's own window still
    /// shows it (`signal_window_start`) -- a stale selection the
    /// window has long since scrolled past seeds from Now instead,
    /// never teleporting the window back to it.
    pub fn signal_scrub(&mut self, delta: i64) {
        let now = self.signal_now_col.unwrap_or(0);
        let drawn_in_window = self.signal_drawn_col.filter(|&c| {
            self.signal_window_start
                .map(|start| c >= start)
                .unwrap_or(true)
        });
        let current = self.signal_cursor_col.or(drawn_in_window).unwrap_or(now) as i64;
        self.signal_cursor_col = Some(current.saturating_add(delta).clamp(0, now as i64) as usize);
    }

    /// `n` (§4): the cursor returns to Now.
    pub fn signal_now(&mut self) {
        self.signal_cursor_col = None;
    }

    /// ←→'s own step on the Flow rail (issue #691): one station's
    /// worth of characters -- `tui::draw_flow` clamps the real value
    /// against the rail's own width, the only place that knows it.
    const FLOW_SCROLL_STEP: usize = 10;

    /// A row's own path is a loop pass's (issue #677's
    /// `rail::is_pass_path`, not `RowKind::Pass`): a loop whose body
    /// is a single container op (an `if`, another loop, a `dynamic`)
    /// places that op's own entry directly at the pass's own path,
    /// with no separate `Pass`-kind row there at all.
    fn row_is_pass(path: &str) -> bool {
        match path.rsplit_once('.') {
            Some((parent, _)) => oscilloscope_core::rail::is_pass_path(path, parent),
            None => false,
        }
    }

    /// ↑↓ on the Flow screen (issue #691, §4): moves the selected
    /// lane over every loop pass the run has, in plan order -- the
    /// rail's own lane concept, read off `self.flow_rows` (the
    /// rail's own unfolded rows), never the tree pane's own folded,
    /// filtered list, or a pass folding absorbs into a breadcrumb
    /// (still on the rail) would be unreachable.
    pub fn flow_select_lane(&mut self, delta: i32) {
        let paths: Vec<&str> = self
            .flow_rows
            .iter()
            .filter(|r| Self::row_is_pass(&r.path))
            .map(|r| r.path.as_str())
            .collect();
        if paths.is_empty() {
            return;
        }
        let idx = self
            .selected_path
            .as_deref()
            .and_then(|p| paths.iter().position(|q| *q == p));
        let next = match idx {
            Some(i) => (i as i32 + delta).clamp(0, paths.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => paths.len() - 1,
        };
        self.select_tree_path(Some(paths[next].to_string()));
        let rows: Vec<&Row> = self.flow_rows.iter().collect();
        self.follow = self.lands_back_on_the_followed_row(&rows);
    }

    /// `←→` on the Flow screen with follow off or the run ended
    /// (issue #704 rule 3): moves over the selected lane's own
    /// stations, in plan order (`self.flow_lane_stations`, refreshed
    /// every redraw against the current selection) -- a plain
    /// station, a decision (named or inferred) or a fork's own host
    /// (a loop, a parallel `dynamic`) are all stations here, never
    /// one of a fork's own lanes, which `↑↓` alone still reaches.
    pub fn flow_select_station(&mut self, delta: i32) {
        if self.flow_lane_stations.is_empty() {
            return;
        }
        let idx = self
            .selected_path
            .as_deref()
            .and_then(|p| self.flow_lane_stations.iter().position(|q| q.as_str() == p));
        // Issue #704 rule 3: already at the selected lane's own end
        // in the direction of travel -- step out of it instead of
        // clamping in place, so ←→ can always leave a lane.
        if let Some(i) = idx {
            let last = self.flow_lane_stations.len() - 1;
            if delta < 0 && i == 0 {
                if let Some(target) = self.flow_step_out_before.clone() {
                    self.select_tree_path(Some(target));
                    self.follow = false;
                }
                return;
            }
            if delta > 0 && i == last {
                if let Some(target) = self.flow_step_out_after.clone() {
                    self.select_tree_path(Some(target));
                    self.follow = false;
                }
                return;
            }
        }
        // Issue #704 rule 3: a bare pass path (`↑↓`'s own selection,
        // never one of `flow_lane_stations`' own entries) is already
        // at its own zone's own edge in both directions at once --
        // step out to the fork host's own neighbour the same way,
        // rather than entering the pass's own first/last station the
        // way landing here from some other screen's unrelated
        // selection still should.
        if idx.is_none() {
            let step_out = if delta >= 0 {
                self.flow_step_out_after.clone()
            } else {
                self.flow_step_out_before.clone()
            };
            if let Some(target) = step_out {
                self.select_tree_path(Some(target));
                self.follow = false;
                return;
            }
        }
        let next = match idx {
            Some(i) => {
                (i as i32 + delta).clamp(0, self.flow_lane_stations.len() as i32 - 1) as usize
            }
            None if delta >= 0 => 0,
            None => self.flow_lane_stations.len() - 1,
        };
        self.select_tree_path(Some(self.flow_lane_stations[next].clone()));
        self.follow = false;
    }

    /// `[`/`]` on the full-screen step (issue #704 rule 4): the
    /// previous/next effect in plan order, skipping a row that has
    /// not started -- over `self.flow_rows` (`render::RenderState::
    /// unfolded_rows`, refreshed every tick regardless of which
    /// screen is on display, so every effect the run plans is here,
    /// not just whichever screen opened the full-screen view).
    pub fn fullscreen_step(&mut self, delta: i32) {
        use oscilloscope_core::model::StatusKind;
        use oscilloscope_core::render::RowKind;
        let candidates: Vec<&str> = self
            .flow_rows
            .iter()
            .filter(|r| {
                !matches!(
                    r.kind,
                    RowKind::Root | RowKind::Pass | RowKind::QuickSteps | RowKind::Report
                )
            })
            .filter(|r| !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped))
            .map(|r| r.path.as_str())
            .collect();
        if candidates.is_empty() {
            return;
        }
        let idx = self
            .selected_path
            .as_deref()
            .and_then(|p| candidates.iter().position(|q| *q == p));
        let next = match idx {
            Some(i) => (i as i32 + delta).clamp(0, candidates.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => candidates.len() - 1,
        };
        self.select_tree_path(Some(candidates[next].to_string()));
        // Issue #704 rule 4: like every other way of moving the
        // shared selection, `[`/`]` turns follow off -- otherwise a
        // live run's own next tick (`main.rs`'s own follow update)
        // reverts the step before it is ever seen, and on the Flow,
        // `default_follow_path` keeps showing instead regardless of
        // where `[`/`]` just stepped to.
        self.follow = false;
    }

    /// The fork `+`/`-` act on (issue #677 §9.3's "the selected
    /// fork's own lanes", issue #691's own follow-up): the nearest
    /// ancestor of the current selection that is itself a fork host
    /// (a loop, or a parallel `dynamic`) on `self.flow_rows` -- never
    /// a fallback to the first loop on the rail once the selection
    /// isn't inside one, which used to let `+`/`-` silently act on
    /// an unrelated fork.
    fn selected_fork_path(&self) -> Option<String> {
        let mut candidate = self.selected_path.as_deref()?;
        loop {
            if self.flow_rows.iter().any(|r| {
                r.path == candidate
                    && matches!(
                        r.kind,
                        oscilloscope_core::render::RowKind::Loop
                            | oscilloscope_core::render::RowKind::Dynamic
                    )
            }) {
                return Some(candidate.to_string());
            }
            candidate = candidate.rsplit_once('.')?.0;
        }
    }

    /// The fork at `fork_path`'s own real lane count: a loop's own
    /// known total (or however many passes are observed so far
    /// without one), or a parallel `dynamic`'s own named children --
    /// the plan already knows that one in full, every immediate
    /// child row directly under the fork's own path.
    fn fork_lane_count(&self, fork_path: &str) -> u64 {
        let is_loop = self
            .flow_rows
            .iter()
            .any(|r| r.path == fork_path && r.kind == oscilloscope_core::render::RowKind::Loop);
        if is_loop {
            let observed = self
                .flow_rows
                .iter()
                .filter(|r| {
                    Self::row_is_pass(&r.path) && r.path.starts_with(&format!("{fork_path}.iter_"))
                })
                .count() as u64;
            self.flow_rows
                .iter()
                .find(|r| r.path == fork_path)
                .and_then(|r| r.loop_progress)
                .and_then(|p| p.total)
                .unwrap_or(observed)
                .max(observed)
        } else {
            let prefix = format!("{fork_path}.");
            self.flow_rows
                .iter()
                .filter(|r| {
                    r.path
                        .strip_prefix(&prefix)
                        .is_some_and(|rest| !rest.contains('.'))
                })
                .count() as u64
        }
    }

    /// `+` (issue #691, §2.3): opens the selected fork's own lanes
    /// three at a time, up to its own real lane count.
    pub fn flow_open_lanes(&mut self) {
        let Some(fork_path) = self.selected_fork_path() else {
            return;
        };
        let lane_count = self.fork_lane_count(&fork_path);
        let entry = self.flow_lanes_open.entry(fork_path).or_insert(0);
        *entry = ((*entry as u64 + 3).min(lane_count)) as usize;
    }

    /// `-` (issue #691): folds the selected fork's own lanes back
    /// into its own band.
    pub fn flow_fold_lanes(&mut self) {
        if let Some(loop_path) = self.selected_fork_path() {
            self.flow_lanes_open.insert(loop_path, 0);
        }
    }

    /// The orchestration pane's own selection, resetting the details
    /// pane back to its top whenever it actually changes (issue
    /// #527: "returns to the top whenever the selected row changes")
    /// — every caller that moves `selected_path`, including follow's
    /// own target update in `main.rs`, goes through this rather than
    /// setting the field directly.
    pub fn select_tree_path(&mut self, path: Option<String>) {
        if self.selected_path != path {
            self.selected_path = path;
            self.details_scroll = 0;
            self.details_source = Pane::Tree;
            // Issue #704 rule 2: the selection moved by some route
            // other than the Signal's own scrub (which writes the
            // selection back through `select_tree_path_from_scrub`
            // below, never this one) -- a stale scrub column must
            // not keep overriding it on the next redraw, so this is
            // the one place every other mover shares to drop it.
            self.signal_cursor_col = None;
        }
    }

    /// The Signal's own scrub (`tui::draw_signal`) writing the shared
    /// selection back to whatever step the cursor actually landed on
    /// -- like `select_tree_path`, but it must never clear `signal_
    /// cursor_col` itself, the very thing that is driving this call.
    pub(crate) fn select_tree_path_from_scrub(&mut self, path: Option<String>) {
        if self.selected_path != path {
            self.selected_path = path;
            self.details_scroll = 0;
            self.details_source = Pane::Tree;
        }
    }

    /// Moves the orchestration pane's selection the way follow, folding
    /// and the first selection of a run do: like `select_tree_path`,
    /// but it never takes the details pane away from a state-pane
    /// selection being read (#610). The details pane switches source
    /// only when the user moves a selection; its scroll resets here
    /// only when it was already showing the tree's selection.
    pub fn select_tree_path_keeping_details(&mut self, path: Option<String>) {
        if self.selected_path != path {
            self.selected_path = path;
            if self.details_source == Pane::Tree {
                self.details_scroll = 0;
            }
            // Same reasoning as `select_tree_path`'s own: follow's
            // own target update (`main.rs`) and `tape_select_run_
            // end_once` both move the selection through here, and a
            // stale scrub column must give way to either.
            self.signal_cursor_col = None;
        }
    }

    /// `rows` is the already-visible list this frame drew (collapsed
    /// subtrees and the errors-only/filter views already applied,
    /// `visible_rows` below) — navigation and collapse/expand act on
    /// exactly what the user can see, in the order they see it.
    /// `has_prompt_sent` is whether the currently selected row's own
    /// details carry a `prompt_sent`: `v` opens that first when
    /// there is one, since it's usually the more useful of the two
    /// to read in full, and the value otherwise.
    /// `store_rows` is the Store screen's own Largest Stores table,
    /// in its own rank order (issue #695) -- empty for every caller
    /// that doesn't care about it (every screen but Store never
    /// reads it at all). It is one frame stale at worst, the same
    /// way `rows`/`state_rows` already are.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        rows: &[&Row],
        state_rows: &[&StateRow],
        running: bool,
        has_prompt_sent: bool,
        store_rows: &[String],
    ) -> Action {
        // Finding K3: raw mode turns a real Ctrl-C into `Char('c')`
        // with the CONTROL modifier, not a distinct key code — caught
        // here, before any dialog gets a look at it, so it's never
        // merely a dismiss (the `Dialog::Help`/`FullValue` arms below
        // close on *any* key) or, worse, the very thing that opens or
        // confirms the plain `c`/`q` confirm dialog instead of
        // cancelling at once.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.dialog = Dialog::None;
            return Action::CtrlC;
        }
        match &mut self.dialog {
            Dialog::ConfirmCancel { quit_after } => {
                let quit_after = *quit_after;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.dialog = Dialog::None;
                        if quit_after && self.is_watch {
                            // Watch owns no engine to cancel; this
                            // confirm only ever guarded a plain quit.
                            Action::Quit
                        } else {
                            if quit_after {
                                self.quit_when_finished = true;
                            }
                            Action::Cancel
                        }
                    }
                    _ => {
                        self.dialog = Dialog::None;
                        Action::None
                    }
                }
            }
            // Issue #714: `T` still cycles the theme with Help
            // open -- every other key dismisses it, same as every
            // other overlay, but `T` on every other screen leaves
            // whatever's open where it is, and Help overlaying one
            // screen's own keys is no different.
            Dialog::Help if key.code == KeyCode::Char('T') => Action::CycleTheme,
            Dialog::Help => {
                self.dialog = Dialog::None;
                Action::None
            }
            // Finding 5: `v` again, while this overlay is already
            // open, cycles between the prompt and the value instead
            // of closing it — any other key still closes it, same as
            // every other overlay.
            Dialog::FullValue(field) => {
                if key.code == KeyCode::Char('v') {
                    *field = match field {
                        FullValueField::Value => FullValueField::PromptSent,
                        FullValueField::PromptSent => FullValueField::Value,
                    };
                } else {
                    self.dialog = Dialog::None;
                }
                Action::None
            }
            Dialog::FilterInput(buf) => {
                match key.code {
                    KeyCode::Enter => {
                        let text = buf.clone();
                        self.filter = if text.is_empty() { None } else { Some(text) };
                        self.dialog = Dialog::None;
                    }
                    KeyCode::Esc => {
                        self.dialog = Dialog::None;
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => {
                        buf.push(c);
                    }
                    _ => {}
                }
                Action::None
            }
            Dialog::None => {
                self.handle_key_normal(key, rows, state_rows, running, has_prompt_sent, store_rows)
            }
        }
    }

    fn handle_key_normal(
        &mut self,
        key: KeyEvent,
        rows: &[&Row],
        state_rows: &[&StateRow],
        running: bool,
        has_prompt_sent: bool,
        store_rows: &[String],
    ) -> Action {
        // Issue #704 rule 4: while the full-screen step is open, its
        // own keys take over first, before any screen's own bindings
        // below (including the Store's own table navigation just
        // below) -- `[`/`]` step to the previous/next effect in plan
        // order (skipping rows that have not started), ↑↓/PgUp/PgDn/
        // Home/End scroll its content, `esc` closes it. `0`-`3`, `?`,
        // `c`/`q` and Ctrl-C still fall through to the match below
        // unchanged, so switching screens or quitting still works.
        if self.tape_fullscreen_step {
            match key.code {
                KeyCode::Esc => {
                    self.tape_fullscreen_step = false;
                    return Action::None;
                }
                // Issue #704 rule 4: `enter` on a pass opens it --
                // the pass whose own line in the Loop section is at
                // or just below the current scroll position, or the
                // last one once scrolled past all of them. Consumed
                // here only when the open step's own detail is
                // actually a loop's; any other `enter` falls through
                // unchanged to the per-screen toggle below, which
                // closes the full-screen view.
                KeyCode::Enter if !self.loop_pass_line_paths.is_empty() => {
                    let target = self
                        .loop_pass_line_paths
                        .iter()
                        .find(|(line, _)| *line >= self.details_scroll)
                        .or_else(|| self.loop_pass_line_paths.last())
                        .map(|(_, path)| path.clone());
                    if let Some(target) = target {
                        self.select_tree_path(Some(target));
                        self.details_scroll = 0;
                    }
                    return Action::None;
                }
                KeyCode::Char('[') => {
                    self.fullscreen_step(-1);
                    return Action::None;
                }
                KeyCode::Char(']') => {
                    self.fullscreen_step(1);
                    return Action::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll_details(-1);
                    return Action::None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll_details(1);
                    return Action::None;
                }
                KeyCode::PageUp => {
                    let page = self.details_viewport.max(1) as i32;
                    self.scroll_details(-page);
                    return Action::None;
                }
                KeyCode::PageDown => {
                    let page = self.details_viewport.max(1) as i32;
                    self.scroll_details(page);
                    return Action::None;
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.details_scroll = 0;
                    return Action::None;
                }
                KeyCode::End | KeyCode::Char('G') => {
                    self.details_scroll = usize::MAX;
                    return Action::None;
                }
                _ => {}
            }
        }
        // Issue #695: the Store screen's own keys (↑↓ and aliases to
        // select a table row, `⏎` to open it) take over first, before
        // any of the generic panes' own bindings below -- `0`-`3`
        // (screen switching), `?`, `c`/`q` and Ctrl-C still fall
        // through to the match below unchanged.
        if self.screen == Screen::Store && self.handle_store_key(key, store_rows) {
            return Action::None;
        }
        // Issue #698 (§2.5): `esc` on a run opened from the Select
        // screen returns there, before any of this screen's own
        // bindings below -- checked here rather than added as a
        // `KeyCode::Esc` arm further down, since every screen's own
        // Store/Flow/Signal guards above a plain match arm would
        // otherwise need to repeat the same `opened_from_select`
        // check themselves.
        if key.code == KeyCode::Esc && self.opened_from_select {
            return Action::BackToSelect;
        }
        match key.code {
            // DESIGN.md §6.3/issue #527: every one of these moves (or
            // scrolls) whichever pane currently has focus — the
            // orchestration pane's own selection when `Pane::Tree`,
            // turning follow off (a manual move always does); the
            // state pane's own, independent selection when
            // `Pane::State`; the details pane's own scroll position
            // when `Pane::Details`.
            // Issue #691 (design v1 part 3, §4): ↑↓ select a lane
            // (a loop's own pass) on the Flow screen, over every
            // `RowKind::Pass` the run has, in plan order -- never
            // `move_or_scroll`'s own tree-shaped walk, which the
            // rail replaces entirely.
            KeyCode::Up | KeyCode::Char('k') if self.screen == Screen::Flow => {
                self.flow_select_lane(-1)
            }
            KeyCode::Down | KeyCode::Char('j') if self.screen == Screen::Flow => {
                self.flow_select_lane(1)
            }
            // PageUp/PageDown/Home/End/g/G have no tree to page or
            // jump through on the Flow screen (the rail replaces it
            // entirely) -- a no-op there, never `move_or_scroll`'s
            // or `jump`'s own tree-shaped walk, and never touching
            // follow either.
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                if self.screen == Screen::Flow => {}
            KeyCode::Char('g') | KeyCode::Char('G') if self.screen == Screen::Flow => {}
            KeyCode::Up | KeyCode::Char('k') => self.move_or_scroll(rows, state_rows, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_or_scroll(rows, state_rows, 1),
            KeyCode::PageUp => {
                let page = self.page_size() as i32;
                self.move_or_scroll(rows, state_rows, -page);
            }
            KeyCode::PageDown => {
                let page = self.page_size() as i32;
                self.move_or_scroll(rows, state_rows, page);
            }
            KeyCode::Home | KeyCode::Char('g') => self.jump(rows, state_rows, true),
            KeyCode::End | KeyCode::Char('G') => self.jump(rows, state_rows, false),
            // Issue #688 (design v1 part 2, §4): ←→ scrub the Signal's
            // own cursor, `+`/`-` zoom its column width, `n` returns
            // the cursor to Now.
            KeyCode::Left if self.screen == Screen::Signal => self.signal_scrub(-1),
            KeyCode::Right if self.screen == Screen::Signal => self.signal_scrub(1),
            KeyCode::Char('n') if self.screen == Screen::Signal => self.signal_now(),
            KeyCode::Char('+') if self.screen == Screen::Signal => self.signal_zoom_in(),
            KeyCode::Char('-') if self.screen == Screen::Signal => self.signal_zoom_out(),
            // Issue #691 (design v1 part 3, §4): ←→ scroll the rail
            // while actually following a live run, turning follow
            // off -- `tui::draw_flow` is the only thing that knows
            // the rail's own real width, so it (not this key
            // handler) turns follow back on once a manual scroll
            // lands Now back in view, the same way the Tape's own
            // selection rule already does.
            KeyCode::Left if self.screen == Screen::Flow && self.follow && running => {
                self.flow_scroll = self.flow_scroll.saturating_sub(Self::FLOW_SCROLL_STEP);
                self.follow = false;
                self.flow_manual_scroll = true;
            }
            KeyCode::Right if self.screen == Screen::Flow && self.follow && running => {
                self.flow_scroll = self.flow_scroll.saturating_add(Self::FLOW_SCROLL_STEP);
                self.follow = false;
                self.flow_manual_scroll = true;
            }
            // Issue #704 rule 3: with follow off, or after the run,
            // ←→ instead move the selection between the selected
            // lane's own stations, in plan order.
            KeyCode::Left if self.screen == Screen::Flow => self.flow_select_station(-1),
            KeyCode::Right if self.screen == Screen::Flow => self.flow_select_station(1),
            // Issue #691: `+` opens the selected fork's own lanes
            // (three at a time, DESIGN.md §2.3's folded loop), `-`
            // folds them back into the band.
            KeyCode::Char('+') if self.screen == Screen::Flow => self.flow_open_lanes(),
            KeyCode::Char('-') if self.screen == Screen::Flow => self.flow_fold_lanes(),
            // Issue #691: ⏎ opens the selected station or pass
            // full-screen, the same toggle the Tape and Signal
            // already share -- only while something actually is
            // selected (`flow_has_selection`, the same guard Signal's
            // own `signal_has_step` is), or it would arm with
            // nothing to open yet and spring open later, unasked.
            KeyCode::Enter if self.screen == Screen::Flow && self.flow_has_selection => {
                self.tape_fullscreen_step = !self.tape_fullscreen_step;
            }
            // Issue #678 (design v1 §4): ⏎ on the Tape, or on Signal,
            // opens the selected row's own step full-screen -- `esc`
            // or ⏎ again returns to the list. Signal only toggles
            // this on while there's actually a step under the
            // cursor to show (`signal_has_step`, the latest redraw's
            // own answer) -- otherwise the toggle would stay armed
            // with nothing to open yet, and spring the full-screen
            // view open later, unasked, the moment a step appears.
            KeyCode::Enter if self.screen == Screen::Tape => {
                self.tape_fullscreen_step = !self.tape_fullscreen_step;
            }
            KeyCode::Enter if self.screen == Screen::Signal && self.signal_has_step => {
                self.tape_fullscreen_step = !self.tape_fullscreen_step;
            }
            // Issue #613: ⏎ on a quick-steps summary row expands it
            // in place -- recognised from the row's own `RowKind`,
            // not by parsing its label.
            KeyCode::Enter if self.pane == Pane::Tree => {
                if let Some(path) = self.selected_path.clone() {
                    let is_quick_steps = rows.iter().any(|r| {
                        r.path == path && r.kind == oscilloscope_core::render::RowKind::QuickSteps
                    });
                    if is_quick_steps {
                        self.expanded_quick_steps.insert(path);
                    }
                }
            }
            // Issue #535: the state pane collapses/expands with the
            // same two keys, on its own independent collapsed set.
            KeyCode::Left if self.pane == Pane::State => {
                if let Some(path) = &self.state_selected_path {
                    self.state_collapsed.insert(path.clone());
                }
            }
            KeyCode::Right if self.pane == Pane::State => {
                if let Some(path) = &self.state_selected_path {
                    self.state_collapsed.remove(path);
                }
            }
            // Issue #626, #632, #650: `0`-`3` all switch screens now.
            // Selection, follow, folding, filters and collapsed rows
            // are all outside `App::screen`, so none of them reset
            // here.
            KeyCode::Char(c @ '0'..='3') => {
                if let Some(screen) = Screen::from_key(c) {
                    // The Tape's and Signal's own ⏎ toggle is shared
                    // state, not per-screen -- without this, opening
                    // it on one and switching screens opens the
                    // other's full-screen view too.
                    if screen != self.screen {
                        self.tape_fullscreen_step = false;
                        // Issue #704 rule 4: the detail pane's own
                        // scroll is shared state too (the Tape's
                        // normal pane, the Flow's Selected pane, the
                        // Signal's and Store's own full-screen step)
                        // -- without this, a scroll left over from
                        // one screen's own pane shows up, mid-
                        // scroll, in whichever unrelated pane the
                        // next screen draws.
                        self.details_scroll = 0;
                    }
                    self.screen = screen;
                }
            }
            // Issue #678: `T` cycles the theme (moved from `t`,
            // which the Tape now uses for its own token column);
            // `t`/`p` below are no-ops on every other screen.
            KeyCode::Char('T') => return Action::CycleTheme,
            KeyCode::Char('t') if self.screen == Screen::Tape => {
                self.tape_tokens_view = !self.tape_tokens_view;
            }
            KeyCode::Char('p') if self.screen == Screen::Tape => {
                self.tape_previews_on = !self.tape_previews_on;
            }
            // Issue #704 rule 4: `m` unfolds the selected step's own
            // Meta section -- the same key on every screen, since
            // there is only one selection to unfold it for.
            KeyCode::Char('m') => self.step_meta_unfolded = !self.step_meta_unfolded,
            KeyCode::Char('f') => self.follow = !self.follow,
            // Issue #695: `e`/`/` act on the Tape's own rows alone
            // (§4); gone from the Store, which has no rows of its own
            // to filter or restrict to errors.
            KeyCode::Char('e') if self.screen != Screen::Store => {
                self.errors_only = !self.errors_only;
            }
            KeyCode::Char('/') if self.screen != Screen::Store => {
                self.dialog = Dialog::FilterInput(self.filter.clone().unwrap_or_default());
            }
            // Gone from the Tape the same way `z` is above -- its own
            // detail pane already shows the prompt and reply in full
            // -- and from the Flow and Store screens too.
            KeyCode::Char('v')
                if self.screen != Screen::Tape
                    && self.screen != Screen::Flow
                    && self.screen != Screen::Store =>
            {
                self.dialog = Dialog::FullValue(if has_prompt_sent {
                    FullValueField::PromptSent
                } else {
                    FullValueField::Value
                });
            }
            KeyCode::Char('?') => self.dialog = Dialog::Help,
            KeyCode::Char('c') => {
                if running {
                    self.dialog = Dialog::ConfirmCancel { quit_after: false };
                }
            }
            KeyCode::Char('q') => {
                if running && !self.is_watch {
                    self.dialog = Dialog::ConfirmCancel { quit_after: true };
                } else {
                    return Action::Quit;
                }
            }
            _ => {}
        }
        Action::None
    }

    /// `PgUp`/`PgDn`'s own page size (issue #527): whichever pane
    /// currently has focus, from the interior height `tui::draw` last
    /// rendered it into (one frame stale at worst — a resize always
    /// forces a redraw before the next key lands).
    fn page_size(&self) -> usize {
        match self.pane {
            Pane::Tree => self.tree_viewport.max(1),
            Pane::State => self.state_viewport.max(1),
            Pane::Details => self.details_viewport.max(1),
        }
    }

    /// The Store screen's own keys (issue #695), tried before any of
    /// the generic panes' own bindings; `true` means the key was
    /// consumed here and `handle_key_normal`'s own match below must
    /// not also act on it.
    fn handle_store_key(&mut self, key: KeyEvent, rows: &[String]) -> bool {
        /// The table's own usual visible row count (DESIGN.md §2.4),
        /// since the Store has no viewport of its own cached from a
        /// real draw the way the Tree/State/Details panes do.
        const STORE_PAGE_SIZE: i32 = 4;
        match key.code {
            // Issue #704 rule 2: the Store's own cursor is the shared
            // selection now, so a manual move here must turn follow
            // off the same way every other screen's own movement
            // already does -- otherwise the very next tick's own
            // follow update (main.rs) overwrites it right back to
            // whatever is running, undoing the move before it is
            // ever seen.
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_store_selection(rows, -1);
                self.follow = false;
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_store_selection(rows, 1);
                self.follow = false;
                true
            }
            KeyCode::PageUp => {
                self.move_store_selection(rows, -STORE_PAGE_SIZE);
                self.follow = false;
                true
            }
            KeyCode::PageDown => {
                self.move_store_selection(rows, STORE_PAGE_SIZE);
                self.follow = false;
                true
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if let Some(first) = rows.first() {
                    self.select_tree_path(Some(first.clone()));
                }
                self.follow = false;
                true
            }
            KeyCode::End | KeyCode::Char('G') => {
                if let Some(last) = rows.last() {
                    self.select_tree_path(Some(last.clone()));
                }
                self.follow = false;
                true
            }
            // `⏎` opens the selected store's own step full-screen
            // (DESIGN.md §2.4), the same toggle the Tape/Signal/Flow
            // already share -- gated on the table actually having a
            // row to open at all, never on `store_cursor_index`
            // resolving a path (it falls back to the first row on
            // screen the same way `move_store_selection` already
            // does, so `enter` opens whatever is actually drawn as
            // the cursor, not nothing). Consumed here either way so
            // it never falls through to the generic pane's own
            // quick-steps expand below.
            KeyCode::Enter => {
                if !rows.is_empty() {
                    self.tape_fullscreen_step = !self.tape_fullscreen_step;
                }
                true
            }
            _ => false,
        }
    }

    /// Issue #704 rule 2: the shared `selected_path`'s own index in
    /// the table's own order, resolved the same way `store::
    /// store_cursor_path` resolves a cursor for drawing -- itself
    /// when it names a row the table already has, else the nearest
    /// ancestor that is one, else the nearest row in plan order
    /// (`self.flow_rows`, the run's own plan-ordered rows, never the
    /// table's own tokens-descending order). `None` only when
    /// nothing at all resolves, which `move_store_selection` and the
    /// `enter` handler both fall back from to the first row on
    /// screen, the same way `store::build_store`'s own cursor does.
    fn store_cursor_index(&self, rows: &[String]) -> Option<usize> {
        let selected_path = self.selected_path.as_deref()?;
        let mut candidate = selected_path;
        loop {
            if let Some(idx) = rows.iter().position(|r| r == candidate) {
                return Some(idx);
            }
            match candidate.rsplit_once('.') {
                Some((parent, _)) => candidate = parent,
                None => break,
            }
        }
        let plan_order: Vec<String> = self.flow_rows.iter().map(|r| r.path.clone()).collect();
        if let Some(path) =
            oscilloscope_core::store::nearest_path_in_plan_order(selected_path, rows, &plan_order)
        {
            return rows.iter().position(|r| *r == path);
        }
        None
    }

    /// `↑`/`↓` select rows in the table's own order (DESIGN.md §2.4:
    /// tokens descending, ties in plan order) -- `rows` is already in
    /// that order (`store::build_store_table`'s own output), so
    /// moving the selection one step always matches moving to the row
    /// actually next to it on screen.
    fn move_store_selection(&mut self, rows: &[String], delta: i32) {
        if rows.is_empty() {
            return;
        }
        // The Store screen's own cursor always resolves to some row
        // (the same fallback-to-first-row `store::build_store` itself
        // uses once nothing in `self.selected_path`'s own lineage has
        // tokens at all) -- arrow keys move from THAT position, or a
        // first press while unresolved would just resync to the row
        // already on screen instead of actually moving.
        let current = self.store_cursor_index(rows).unwrap_or(0);
        let next = (current as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize;
        self.select_tree_path(Some(rows[next].clone()));
    }

    /// Issue #678's own addition for the Tape: a manual move turns
    /// follow off (the shared Overview's own plain rule, kept for
    /// every other screen) unless it lands the selection right back
    /// on the row follow would itself have picked -- `↑↓` past the
    /// Now band and back, say, restores it on its own rather than
    /// needing `f`.
    pub(crate) fn lands_back_on_the_followed_row(&self, rows: &[&Row]) -> bool {
        match self.screen {
            Screen::Tape => {
                use oscilloscope_core::model::StatusKind;
                let target = rows
                    .iter()
                    .filter(|r| matches!(r.status.kind, StatusKind::Running))
                    .max_by_key(|r| r.depth)
                    .or_else(|| {
                        rows.iter()
                            .filter(|r| {
                                matches!(
                                    r.status.kind,
                                    StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                                )
                            })
                            .max_by_key(|r| r.depth)
                    })
                    .map(|r| r.path.as_str());
                target.is_some() && target == self.selected_path.as_deref()
            }
            // The Flow screen's own default pass (the Selected
            // pane's own fallback, `screens::default_pass_path`'s
            // own rule) is the row `↑↓` is really "following" there
            // -- the running pass, or else the first one in plan
            // order.
            Screen::Flow => {
                use oscilloscope_core::model::StatusKind;
                let target = rows
                    .iter()
                    .find(|r| {
                        r.kind == oscilloscope_core::render::RowKind::Pass
                            && r.status.kind == StatusKind::Running
                    })
                    .or_else(|| {
                        rows.iter()
                            .find(|r| r.kind == oscilloscope_core::render::RowKind::Pass)
                    })
                    .map(|r| r.path.as_str());
                target.is_some() && target == self.selected_path.as_deref()
            }
            _ => false,
        }
    }

    fn move_or_scroll(&mut self, rows: &[&Row], state_rows: &[&StateRow], delta: i32) {
        match self.pane {
            Pane::Tree => {
                self.move_tree_selection(rows, delta);
                self.follow = self.lands_back_on_the_followed_row(rows);
            }
            Pane::State => self.move_state_selection(state_rows, delta),
            Pane::Details => self.scroll_details(delta),
        }
    }

    fn jump(&mut self, rows: &[&Row], state_rows: &[&StateRow], to_first: bool) {
        match self.pane {
            Pane::Tree => {
                if !rows.is_empty() {
                    let target = if to_first {
                        rows[0]
                    } else {
                        rows[rows.len() - 1]
                    };
                    self.select_tree_path(Some(target.path.clone()));
                }
                self.follow = false;
            }
            Pane::State => {
                if !state_rows.is_empty() {
                    let target = if to_first {
                        state_rows[0]
                    } else {
                        state_rows[state_rows.len() - 1]
                    };
                    self.select_state_path(Some(target.path.clone()));
                }
            }
            Pane::Details => {
                self.details_scroll = if to_first { 0 } else { usize::MAX };
            }
        }
    }

    fn scroll_details(&mut self, delta: i32) {
        if delta < 0 {
            self.details_scroll = self.details_scroll.saturating_sub((-delta) as usize);
        } else {
            self.details_scroll = self.details_scroll.saturating_add(delta as usize);
            // `tui::draw` clamps this to the real content length once
            // it knows it; `usize::MAX` (jump-to-end, above) relies on
            // exactly that same clamp.
        }
    }

    fn move_tree_selection(&mut self, rows: &[&Row], delta: i32) {
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected_path
            .as_deref()
            .and_then(|p| rows.iter().position(|r| r.path == p));
        let next = match current {
            Some(idx) => (idx as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => rows.len() - 1,
        };
        self.select_tree_path(Some(rows[next].path.clone()));
    }

    /// The state pane's own, independent selection (issue #527):
    /// never touches `selected_path` or `follow`.
    fn move_state_selection(&mut self, rows: &[&StateRow], delta: i32) {
        if rows.is_empty() {
            return;
        }
        let current = self
            .state_selected_path
            .as_deref()
            .and_then(|p| rows.iter().position(|r| r.path == p));
        let next = match current {
            Some(idx) => (idx as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => rows.len() - 1,
        };
        self.select_state_path(Some(rows[next].path.clone()));
    }

    /// The state pane's own selection, resetting the details pane
    /// back to its top and marking it as what `Details` should now
    /// show (#610) whenever it actually changes -- the same rule
    /// `select_tree_path` already applies for the orchestration
    /// pane's own selection.
    fn select_state_path(&mut self, path: Option<String>) {
        if self.state_selected_path != path {
            self.state_selected_path = path;
            self.details_scroll = 0;
            self.details_source = Pane::State;
        }
    }

    /// `f`'s own target (§6.3): the innermost row osi considers
    /// "happening right now" — a strict `Running` leaf over a
    /// `LikelyRunning`/`RunningOrQueued` guess, and the deepest path
    /// among ties, since a nested running effect is more specific
    /// than its enclosing container. `None` when nothing is running
    /// at all (a pending or finished run) — the caller leaves the
    /// selection exactly where it was then.
    pub fn follow_target(rows: &[Row]) -> Option<&str> {
        use oscilloscope_core::model::StatusKind;

        rows.iter()
            .filter(|r| matches!(r.status.kind, StatusKind::Running))
            .max_by_key(|r| r.depth)
            .or_else(|| {
                rows.iter()
                    .filter(|r| {
                        matches!(
                            r.status.kind,
                            StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                        )
                    })
                    .max_by_key(|r| r.depth)
            })
            .map(|r| r.path.as_str())
    }

    /// [`Self::follow_target`], mapped onto the nearest drawn
    /// ancestor when the target itself is hidden by collapse or the
    /// filter (issue #643's second follow-up): follow must always
    /// land on a row `visible_rows` still draws, or nothing is
    /// highlighted and the next `j`/`k` jumps to the top instead of
    /// moving from where the run actually is. `None` only when
    /// `follow_target` itself is (nothing running), same as before.
    pub fn follow_target_or_nearest_visible(rows: &[Row], app: &App) -> Option<String> {
        let target = Self::follow_target(rows)?;
        let visible: std::collections::BTreeSet<&str> = visible_rows(rows, app)
            .into_iter()
            .map(|r| r.path.as_str())
            .collect();
        let mut candidate = target;
        loop {
            if visible.contains(candidate) {
                return Some(candidate.to_string());
            }
            match candidate.rsplit_once('.') {
                Some((parent, _)) => candidate = parent,
                None => return None,
            }
        }
    }

    /// Keeps selection and `collapsed` meaningful once folding has
    /// changed what `rows` actually draws (issue #613): if the
    /// selected path (or a `collapsed` entry) is no longer among
    /// `rows` at all -- its own container folded into a breadcrumb
    /// mid-run -- this re-points it to the visible row whose path is
    /// that one, or extends it with `.` (the breadcrumb that
    /// absorbed it).
    pub fn reconcile_folding(&mut self, rows: &[Row]) {
        // Issue #704 rule 4: while the full-screen step is open, the
        // selection is never a row to highlight in this (always
        // folded) list -- it names whichever effect `[`/`]` just
        // landed on, resolved against the unfolded rows regardless
        // of what folds here. Reassigning it to the absorbing row
        // the instant a loop pass's quick-steps fold around it would
        // land the very next `[`/`]` back on the first effect (that
        // row's own `RowKind::QuickSteps` is excluded from `App::
        // fullscreen_step`'s own candidates), undoing the step
        // before it is ever seen.
        if !self.tape_fullscreen_step {
            if let Some(selected) = self.selected_path.clone() {
                if !rows.iter().any(|r| r.path == selected) {
                    if let Some(absorbed) = Self::absorbing_row_path(rows, &selected) {
                        self.select_tree_path_keeping_details(Some(absorbed));
                    }
                }
            }
        }
        let stale: Vec<String> = self
            .collapsed
            .iter()
            .filter(|path| !rows.iter().any(|r| &r.path == *path))
            .cloned()
            .collect();
        for path in stale {
            if let Some(absorbed) = Self::absorbing_row_path(rows, &path) {
                self.collapsed.remove(&path);
                self.collapsed.insert(absorbed);
            }
        }
    }

    fn absorbing_row_path(rows: &[Row], path: &str) -> Option<String> {
        let prefix = format!("{path}.");
        rows.iter()
            .find(|r| r.path == path || r.path.starts_with(&prefix))
            .map(|r| r.path.clone())
    }
}

/// Everything currently visible in the tree pane (DESIGN.md §6.3's
/// `e`/`/`/collapse semantics): a collapsed row's own descendants are
/// dropped, and (independently) errors-only/the filter drop any row
/// that doesn't match *and has no matching descendant* — a failed
/// leaf ten levels deep must still pull every one of its ancestors
/// along, or there'd be nothing to show it under.
pub fn visible_rows<'a>(rows: &'a [Row], app: &App) -> Vec<&'a Row> {
    let keep = |r: &&Row| -> bool {
        if app.errors_only
            && !matches_or_has_match(rows, r, &|row| {
                matches!(
                    row.status.kind,
                    oscilloscope_core::model::StatusKind::Failed
                        | oscilloscope_core::model::StatusKind::FailedHandled
                )
            })
        {
            return false;
        }
        if let Some(filter) = &app.filter {
            let filter = filter.to_lowercase();
            if !matches_or_has_match(rows, r, &|row| {
                row.path.to_lowercase().contains(&filter)
                    || row.label.to_lowercase().contains(&filter)
            }) {
                return false;
            }
        }
        true
    };

    let mut out = Vec::new();
    // Finding 10: a trailing `.` so collapsing `prime.fetch` doesn't
    // also hide an unrelated sibling that merely shares its prefix,
    // like `prime.fetch_all` — `starts_with` alone can't tell "is a
    // descendant of" from "happens to start with the same letters".
    let mut hidden_prefix: Option<String> = None;
    for row in rows {
        if let Some(prefix) = &hidden_prefix {
            if row.path.starts_with(prefix.as_str()) {
                continue;
            }
            hidden_prefix = None;
        }
        if app.collapsed.contains(&row.path) {
            hidden_prefix = Some(format!("{}.", row.path));
        }
        if !keep(&row) {
            continue;
        }
        out.push(row);
    }
    out
}

/// The Tape's own movement list (issue #678): one [`Row`] per
/// [`oscilloscope_core::screens::TapeItem::Row`], in the screen's own
/// draw order (finish order, then running, then rows ahead) -- never
/// `rows`' own plan/depth-first order, which puts a loop's later
/// passes and the plan's own root in places the Tape never draws
/// them at all. `depth`/`duration_s`/`loop_progress`/`dim`/
/// `has_children` carry no weight for movement (`move_tree_selection`/
/// `jump` read only `path`), so each synthetic `Row` here is a
/// placeholder beyond `path`/`label`/`kind`/`status`.
pub fn visible_rows_for_tape(
    screen: &oscilloscope_core::screens::TapeScreen,
) -> Vec<oscilloscope_core::render::Row> {
    screen
        .items
        .iter()
        .filter_map(|item| match item {
            oscilloscope_core::screens::TapeItem::Row(row) => {
                Some(oscilloscope_core::render::Row {
                    path: row.path.clone(),
                    depth: 0,
                    label: row.label.clone(),
                    kind: row.kind,
                    status: row.status.clone(),
                    duration_s: None,
                    loop_progress: None,
                    dim: None,
                    has_children: false,
                })
            }
            _ => None,
        })
        .collect()
}

fn matches_or_has_match(rows: &[Row], row: &Row, pred: &dyn Fn(&Row) -> bool) -> bool {
    if pred(row) {
        return true;
    }
    let prefix = format!("{}.", row.path);
    rows.iter().any(|r| r.path.starts_with(&prefix) && pred(r))
}

/// The Select screen's own key/cursor state (issue #698, §2.5/§4):
/// `cursor` indexes `Row::All` + every running row when something's
/// running, or [`oscilloscope_core::select::RecentRow`] otherwise --
/// the same index space [`crate::main`]'s own loop already built the
/// two lists in, never mixed across a refresh that switches between
/// them. Pure: [`Self::handle_key`] takes every list-shaped fact it
/// needs as a plain argument, so a test drives it with no real
/// terminal and no real [`oscilloscope_core::discovery::DiscoveredRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectState {
    pub cursor: usize,
    pub help_open: bool,
}

impl SelectState {
    pub fn new(cursor: usize) -> Self {
        SelectState {
            cursor,
            help_open: false,
        }
    }
}

/// What [`SelectState::handle_key`] decided, for `main.rs`'s own loop
/// to act on -- `AttachRun`/`AttachRecent` carry the chosen row's
/// index into whichever list is current, never the row itself (this
/// module holds neither list, only the cursor into one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKeyOutcome {
    None,
    Quit(u8),
    AttachRun(usize),
    AttachRecent(usize),
    /// `enter` on row 0 (`All`, issue #698's own All screen):
    /// `runs_empty` is never true when this fires -- the empty
    /// Select has no such row at all.
    OpenAll,
    /// `T` (issue #714): cycle the theme, on this screen too.
    CycleTheme,
}

impl SelectState {
    /// One key's worth of the Select screen's own behaviour (issue
    /// #698 §4): `↑↓`/`j`/`k`/`Home`/`End`/
    /// `g`/`G` move and clamp `cursor` at `[0, max_index]`; `enter`
    /// attaches (ignored on `All`, row 0, and on a Recent Runs row
    /// that isn't `openable`); `q` quits at once with exit 0; a raw
    /// Ctrl-C (`Char('c')` with the `CONTROL` modifier -- the same
    /// reason `App::handle_key`'s own doc comment gives: raw mode
    /// turns a real Ctrl-C into this, never a distinct key code)
    /// quits with exit 130, checked before `help_open` gets a look at
    /// it, same as everywhere else this repository handles it; any
    /// other key while `help_open` merely closes it. `0`-`3` and a
    /// plain `c` do nothing here (Select attaches nothing of its own
    /// to switch a screen or cancel).
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        runs_empty: bool,
        max_index: usize,
        cursor_recent_openable: bool,
    ) -> SelectKeyOutcome {
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            self.help_open = false;
            return SelectKeyOutcome::Quit(130);
        }
        // Issue #714: `T` cycles the theme even with Help open,
        // without closing it -- checked before the "any other key
        // closes Help" fallback below, the same way Ctrl-C is.
        if code == KeyCode::Char('T') {
            return SelectKeyOutcome::CycleTheme;
        }
        if self.help_open {
            self.help_open = false;
            return SelectKeyOutcome::None;
        }
        match code {
            KeyCode::Char('q') => SelectKeyOutcome::Quit(0),
            KeyCode::Char('?') => {
                self.help_open = true;
                SelectKeyOutcome::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                SelectKeyOutcome::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(max_index);
                SelectKeyOutcome::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.cursor = 0;
                SelectKeyOutcome::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.cursor = max_index;
                SelectKeyOutcome::None
            }
            KeyCode::Enter => {
                if runs_empty {
                    if cursor_recent_openable {
                        SelectKeyOutcome::AttachRecent(self.cursor)
                    } else {
                        SelectKeyOutcome::None
                    }
                } else if self.cursor > 0 {
                    SelectKeyOutcome::AttachRun(self.cursor - 1)
                } else {
                    // Row 0 is `All`: `enter` opens the All screen
                    // (issue #698).
                    SelectKeyOutcome::OpenAll
                }
            }
            // `0`-`3` and `c` do nothing here (§5: no run attached to
            // switch a screen or cancel); `esc` likewise has nowhere
            // on this screen to go back to.
            _ => SelectKeyOutcome::None,
        }
    }
}

/// The All screen's own key/focus state (issue #698, §2.6/§4):
/// `screen` is shared by every tile at once (`0`-`3`); `focus`
/// indexes the *current page's* own real tiles, `[0, page_run_
/// count)` -- never the grid's own slot count, which also counts a
/// trailing `… n more` tile that nothing can focus. Pure, like
/// [`SelectState`]: a caller resolves `focus` to an absolute run
/// index (`page_offset + focus`) and page transitions on its own,
/// so a test drives every key with no real grid or terminal
/// involved.
#[derive(Debug, Clone, PartialEq)]
pub struct AllState {
    pub screen: oscilloscope_core::screens::Screen,
    pub focus: usize,
    pub page: usize,
    pub help_open: bool,
    /// The shared Signal scale across every run (never 0, and only
    /// ever growing) -- kept here, rather than local to one leg of
    /// the All loop, so it survives `esc` from an opened run back
    /// into a fresh grid instead of collapsing back to its starting
    /// floor.
    pub shared_sent_top: u64,
    pub shared_received_top: u64,
    /// The Combined Signal's own shared scale (issue #698's `u`) --
    /// kept here rather than local to one leg of the Combined loop,
    /// so it survives `enter`/`esc` round trips through an opened
    /// run the same way the grid's own tiles' shared scale does.
    pub combined_sent_top: u64,
    pub combined_received_top: u64,
    /// Set when `u` bounced straight back here because the
    /// terminal was too small for the Combined Signal's own layout
    /// -- shown once on the grid's own status line, then cleared,
    /// the same shape the Signal screen's own bounce to the Tape
    /// below 80 columns already has, just with a note where that
    /// one needs none (a resize can put a user back on the Tape
    /// with no explanation needed; here `u` looked like it did
    /// nothing at all otherwise).
    pub combined_too_small_note: Option<String>,
}

impl AllState {
    pub fn new() -> Self {
        AllState {
            screen: oscilloscope_core::screens::Screen::Tape,
            focus: 0,
            page: 0,
            help_open: false,
            shared_sent_top: oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            shared_received_top: oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            combined_sent_top: oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            combined_received_top: oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            combined_too_small_note: None,
        }
    }
}

impl Default for AllState {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`AllState::handle_key`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllKeyOutcome {
    None,
    Quit(u8),
    /// `enter` on the focused tile -- the caller resolves this
    /// page-relative index to an absolute run index.
    Open(usize),
    /// `esc`: back to Select.
    BackToSelect,
    /// `u`: the Combined Signal.
    Combined,
    /// `T` (issue #714): cycle the theme, on this screen too.
    CycleTheme,
}

impl AllState {
    /// One key's worth of the All screen's own behaviour (issue
    /// #698, §2.6/§4): `0`-`3` set `screen`; `←→↑↓` move `focus`
    /// within the current page's own `columns` x rows grid, wrapping
    /// at the page's own edges (never crossing a page -- that's
    /// `PageUp`/`PageDown`'s own job); `enter` opens
    /// the focused tile; `u` opens the Combined Signal;
    /// `esc` goes back to Select; `q` quits at once
    /// (watching only, never a confirm); a raw Ctrl-C quits with
    /// exit 130, checked first, same reasoning as everywhere else
    /// this repository handles it. `page_run_count` is how many real
    /// tiles the *current* page actually has (at most `columns *
    /// rows`, fewer on a last page that doesn't fill it, or one
    /// fewer still while a `… n more` tile takes the last slot).
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        columns: usize,
        page_run_count: usize,
        page_count: usize,
    ) -> AllKeyOutcome {
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            self.help_open = false;
            return AllKeyOutcome::Quit(130);
        }
        // Issue #714: `T` cycles the theme even with Help open,
        // without closing it -- same placement as `SelectState`'s
        // own.
        if code == KeyCode::Char('T') {
            return AllKeyOutcome::CycleTheme;
        }
        if self.help_open {
            self.help_open = false;
            return AllKeyOutcome::None;
        }
        let columns = columns.max(1);
        let max_focus = page_run_count.saturating_sub(1);
        match code {
            KeyCode::Char('q') => AllKeyOutcome::Quit(0),
            KeyCode::Char('?') => {
                self.help_open = true;
                AllKeyOutcome::None
            }
            KeyCode::Esc => AllKeyOutcome::BackToSelect,
            KeyCode::Char('0') => {
                self.screen = oscilloscope_core::screens::Screen::Tape;
                AllKeyOutcome::None
            }
            KeyCode::Char('1') => {
                self.screen = oscilloscope_core::screens::Screen::Signal;
                AllKeyOutcome::None
            }
            KeyCode::Char('2') => {
                self.screen = oscilloscope_core::screens::Screen::Flow;
                AllKeyOutcome::None
            }
            KeyCode::Char('3') => {
                self.screen = oscilloscope_core::screens::Screen::Store;
                AllKeyOutcome::None
            }
            KeyCode::PageDown => {
                if page_count > 0 {
                    self.page = (self.page + 1) % page_count;
                    self.focus = 0;
                }
                AllKeyOutcome::None
            }
            KeyCode::PageUp => {
                if page_count > 0 {
                    self.page = (self.page + page_count - 1) % page_count;
                    self.focus = 0;
                }
                AllKeyOutcome::None
            }
            KeyCode::Left => {
                if page_run_count > 0 {
                    let col = self.focus % columns;
                    let row = self.focus / columns;
                    let new_col = if col == 0 { columns - 1 } else { col - 1 };
                    self.focus = (row * columns + new_col).min(max_focus);
                }
                AllKeyOutcome::None
            }
            KeyCode::Right => {
                if page_run_count > 0 {
                    let col = self.focus % columns;
                    let row = self.focus / columns;
                    let new_col = (col + 1) % columns;
                    self.focus = (row * columns + new_col).min(max_focus);
                }
                AllKeyOutcome::None
            }
            KeyCode::Up => {
                if page_run_count > 0 {
                    let rows = page_run_count.div_ceil(columns).max(1);
                    let col = self.focus % columns;
                    let row = self.focus / columns;
                    let new_row = if row == 0 { rows - 1 } else { row - 1 };
                    self.focus = (new_row * columns + col).min(max_focus);
                }
                AllKeyOutcome::None
            }
            KeyCode::Down => {
                if page_run_count > 0 {
                    let rows = page_run_count.div_ceil(columns).max(1);
                    let col = self.focus % columns;
                    let row = self.focus / columns;
                    let new_row = (row + 1) % rows;
                    self.focus = (new_row * columns + col).min(max_focus);
                }
                AllKeyOutcome::None
            }
            KeyCode::Enter => {
                if page_run_count > 0 {
                    AllKeyOutcome::Open(self.focus.min(max_focus))
                } else {
                    AllKeyOutcome::None
                }
            }
            // `c` is never offered here (`c` cancels only a run
            // this osi launched, and the focused run's own
            // ownership is a question for the attached single-run
            // screen, not this one).
            KeyCode::Char('u') => AllKeyOutcome::Combined,
            _ => AllKeyOutcome::None,
        }
    }
}

/// The Combined Signal's own cursor state (issue #698, §2.6/§4):
/// `←→` scrub a column cursor, `n` returns it to Now, `↑↓` move the
/// Per Run table's own cursor, `enter` opens the selected run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CombinedState {
    /// `None` while following Now (`n` returns to it) -- a
    /// scrubbed column once `←→` has moved it away.
    pub scrub_col: Option<usize>,
    pub cursor_run: usize,
    pub help_open: bool,
}

impl CombinedState {
    pub fn new() -> Self {
        CombinedState::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombinedKeyOutcome {
    None,
    Quit(u8),
    /// `esc`: back to the tiles.
    BackToTiles,
    /// `enter` on the Per Run table's own cursor.
    Open(usize),
    /// `T` (issue #714): cycle the theme, on this screen too.
    CycleTheme,
}

impl CombinedState {
    /// One key's worth of the Combined Signal's own behaviour
    /// (issue #698, §4): `←→` scrub a column cursor within `[0,
    /// max_col]`, starting from `now_col` the first time; `n`
    /// returns to Now (clears the scrub); `↑↓` move the Per Run
    /// cursor within `run_count`, wrapping at its own edges; `enter`
    /// opens the selected run; `esc` returns to the tiles; `q` quits
    /// at once (watching only, never a confirm); a raw Ctrl-C quits
    /// with exit 130, checked first, same reasoning as everywhere
    /// else this repository handles it.
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        run_count: usize,
        now_col: usize,
        max_col: usize,
    ) -> CombinedKeyOutcome {
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            self.help_open = false;
            return CombinedKeyOutcome::Quit(130);
        }
        // Issue #714: `T` cycles the theme even with Help open,
        // without closing it -- same placement as `SelectState`'s
        // own.
        if code == KeyCode::Char('T') {
            return CombinedKeyOutcome::CycleTheme;
        }
        if self.help_open {
            self.help_open = false;
            return CombinedKeyOutcome::None;
        }
        match code {
            KeyCode::Char('q') => CombinedKeyOutcome::Quit(0),
            KeyCode::Char('?') => {
                self.help_open = true;
                CombinedKeyOutcome::None
            }
            KeyCode::Esc => CombinedKeyOutcome::BackToTiles,
            KeyCode::Left => {
                let current = self.scrub_col.unwrap_or(now_col);
                self.scrub_col = Some(current.saturating_sub(1));
                CombinedKeyOutcome::None
            }
            KeyCode::Right => {
                let current = self.scrub_col.unwrap_or(now_col);
                self.scrub_col = Some((current + 1).min(max_col));
                CombinedKeyOutcome::None
            }
            KeyCode::Char('n') => {
                self.scrub_col = None;
                CombinedKeyOutcome::None
            }
            KeyCode::Up => {
                if run_count > 0 {
                    self.cursor_run = (self.cursor_run + run_count - 1) % run_count;
                }
                CombinedKeyOutcome::None
            }
            KeyCode::Down => {
                if run_count > 0 {
                    self.cursor_run = (self.cursor_run + 1) % run_count;
                }
                CombinedKeyOutcome::None
            }
            KeyCode::Enter => {
                if run_count > 0 {
                    CombinedKeyOutcome::Open(self.cursor_run.min(run_count - 1))
                } else {
                    CombinedKeyOutcome::None
                }
            }
            _ => CombinedKeyOutcome::None,
        }
    }
}

/// Everything currently visible in the state pane (issue #535): a
/// collapsed row's own descendants are dropped, same rule `visible_
/// rows` uses for the orchestration pane's own `app.collapsed` — but
/// against `app.state_collapsed` instead, and with no errors-only/
/// filter view (the state pane has neither).
pub fn visible_state_rows<'a>(rows: &'a [StateRow], app: &App) -> Vec<&'a StateRow> {
    let mut out = Vec::new();
    let mut hidden_prefix: Option<String> = None;
    for row in rows {
        if let Some(prefix) = &hidden_prefix {
            if row.path.starts_with(prefix.as_str()) {
                continue;
            }
            hidden_prefix = None;
        }
        if app.state_collapsed.contains(&row.path) {
            hidden_prefix = Some(format!("{}.", row.path));
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_moves_the_tree_selection_without_taking_the_details_from_the_state_pane() {
        let mut app = App::new(false);
        app.select_tree_path(Some("prime.a".to_string()));
        // The user picks a named report in the state pane and scrolls its details.
        app.select_state_path(Some("status".to_string()));
        app.details_scroll = 5;
        // Follow (or folding) moves the tree selection on the next rebuild.
        app.select_tree_path_keeping_details(Some("prime.b".to_string()));
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        assert_eq!(app.details_source, Pane::State);
        assert_eq!(app.details_scroll, 5);
        // A tree selection the user moves takes the details back.
        app.select_tree_path(Some("prime.c".to_string()));
        assert_eq!(app.details_source, Pane::Tree);
        assert_eq!(app.details_scroll, 0);
    }

    #[test]
    fn follow_resets_the_details_scroll_when_the_details_show_the_tree_selection() {
        let mut app = App::new(false);
        app.select_tree_path(Some("prime.a".to_string()));
        app.details_scroll = 3;
        app.select_tree_path_keeping_details(Some("prime.b".to_string()));
        assert_eq!(app.details_source, Pane::Tree);
        assert_eq!(app.details_scroll, 0);
    }
    use crossterm::event::{KeyEventKind, KeyEventState};
    use oscilloscope_core::model::{RowStatus, StatusKind};
    use oscilloscope_core::render::RowKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn row(path: &str, depth: usize, kind: StatusKind) -> Row {
        Row {
            path: path.to_string(),
            depth,
            label: path.rsplit('.').next().unwrap().to_string(),
            kind: RowKind::Tool,
            status: RowStatus {
                kind,
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
    fn down_then_up_moves_the_selection_and_clamps_at_the_ends() {
        let rows = [
            row("prime.a", 1, StatusKind::Done),
            row("prime.b", 1, StatusKind::Pending),
        ];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        // Already at the end: another Down must not go out of bounds.
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::Up), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
    }

    #[test]
    fn collapsing_a_row_hides_its_descendants_from_visible_rows() {
        let rows = [
            row("prime.loop", 1, StatusKind::Running),
            row("prime.loop.iter_0", 2, StatusKind::Done),
            row("prime.after", 1, StatusKind::Pending),
        ];
        let mut app = App::new(false);
        app.collapsed.insert("prime.loop".to_string());
        let visible = visible_rows(&rows, &app);
        let paths: Vec<&str> = visible.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["prime.loop", "prime.after"]);
    }

    #[test]
    fn collapsing_a_row_does_not_hide_an_unrelated_sibling_with_the_same_prefix() {
        // Finding 10: `prime.fetch_all` merely starts with the same
        // letters as `prime.fetch`, not a descendant of it.
        let rows = [
            row("prime.fetch", 1, StatusKind::Done),
            row("prime.fetch_all", 1, StatusKind::Pending),
        ];
        let mut app = App::new(false);
        app.collapsed.insert("prime.fetch".to_string());
        let visible = visible_rows(&rows, &app);
        let paths: Vec<&str> = visible.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["prime.fetch", "prime.fetch_all"]);
    }

    #[test]
    fn follow_moves_to_the_deepest_running_row() {
        let rows = [
            row("prime.fan", 1, StatusKind::Running),
            row("prime.fan.inner", 2, StatusKind::Running),
            row("prime.fan.done", 2, StatusKind::Done),
        ];
        assert_eq!(App::follow_target(&rows), Some("prime.fan.inner"));
    }

    #[test]
    fn follow_target_is_none_when_nothing_is_running() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        assert_eq!(App::follow_target(&rows), None);
    }

    /// Issue #643's second follow-up: `follow_target`'s own pick
    /// (`prime.fan.inner`) is hidden once its container collapses --
    /// follow must land on `prime.fan` (the nearest row `visible_
    /// rows` still draws) instead of a path nothing highlights.
    #[test]
    fn follow_maps_onto_the_nearest_visible_ancestor_once_collapsed() {
        let rows = [
            row("prime.fan", 1, StatusKind::Running),
            row("prime.fan.inner", 2, StatusKind::Running),
        ];
        let mut app = App::new(false);
        app.collapsed.insert("prime.fan".to_string());
        assert_eq!(
            App::follow_target_or_nearest_visible(&rows, &app),
            Some("prime.fan".to_string())
        );
    }

    /// Same follow-up, for the filter: `prime.fan.inner` (the
    /// follow target) has no match of its own and no matching
    /// descendant, so the filter drops it -- but its container
    /// still has one, through the sibling that does match, so it
    /// stays, and follow lands there instead.
    #[test]
    fn follow_maps_onto_the_nearest_visible_ancestor_once_filtered() {
        let rows = [
            row("prime.fan", 1, StatusKind::Running),
            row("prime.fan.inner", 2, StatusKind::Running),
            row("prime.fan.sibling", 2, StatusKind::Done),
        ];
        let mut app = App::new(false);
        app.filter = Some("sibling".to_string());
        assert_eq!(
            App::follow_target_or_nearest_visible(&rows, &app),
            Some("prime.fan".to_string())
        );
    }

    /// With nothing hidden at all, follow still lands straight on
    /// its own real target, same as `follow_target` alone.
    #[test]
    fn follow_maps_onto_its_own_target_when_nothing_is_hidden() {
        let rows = [
            row("prime.fan", 1, StatusKind::Running),
            row("prime.fan.inner", 2, StatusKind::Running),
        ];
        let app = App::new(false);
        assert_eq!(
            App::follow_target_or_nearest_visible(&rows, &app),
            Some("prime.fan.inner".to_string())
        );
    }

    #[test]
    fn follow_is_on_by_default_and_f_toggles_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        assert!(app.follow, "issue #527: follow is on when the TUI starts");
        app.handle_key(key(KeyCode::Char('f')), &visible, &[], true, false, &[]);
        assert!(!app.follow);
        app.handle_key(key(KeyCode::Char('f')), &visible, &[], true, false, &[]);
        assert!(app.follow);
    }

    /// Issue #613: a container the user selected can fold into a
    /// breadcrumb mid-run (its own status catching up to its child's)
    /// -- the selection must follow it to the breadcrumb row that
    /// absorbed it, not point at a path `rows` no longer has at all.
    #[test]
    fn selection_follows_its_container_folding_into_a_breadcrumb() {
        let mut app = App::new(false);
        app.selected_path = Some("prime.a".to_string());
        app.collapsed.insert("prime.a".to_string());
        let folded_rows = [row("prime.a.b", 1, StatusKind::Done)];
        app.reconcile_folding(&folded_rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a.b"));
        assert!(app.collapsed.contains("prime.a.b"));
        assert!(!app.collapsed.contains("prime.a"));
    }

    #[test]
    fn selection_is_untouched_when_its_own_row_is_still_drawn() {
        let mut app = App::new(false);
        app.selected_path = Some("prime.a".to_string());
        let rows = [row("prime.a", 1, StatusKind::Done)];
        app.reconcile_folding(&rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
    }

    /// Issue #704 rule 4: while the full-screen step is open, the
    /// selection names an effect to show, resolved against the
    /// unfolded rows, never a row to highlight in this (always
    /// folded) list -- reassigning it to the row that absorbed it
    /// would land the very next `]` back on the first effect, since
    /// that absorbing row's own kind (`QuickSteps`) is excluded from
    /// `App::fullscreen_step`'s own candidates.
    #[test]
    fn reconcile_folding_does_nothing_while_the_fullscreen_step_is_open() {
        let mut app = App::new(false);
        app.selected_path = Some("prime.a.b".to_string());
        app.tape_fullscreen_step = true;
        let folded_rows = [row("prime.a", 1, StatusKind::Done)];
        app.reconcile_folding(&folded_rows);
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.a.b"),
            "the full-screen step's own selection must survive a row folding around it"
        );
    }

    #[test]
    fn enter_on_the_tape_toggles_the_fullscreen_step_view() {
        let mut app = App::new(false);
        assert_eq!(app.screen, Screen::Tape, "the Tape is the default screen");
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(!app.tape_fullscreen_step);
    }

    #[test]
    fn enter_on_an_ordinary_row_does_not_expand_anything() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.selected_path = Some("prime.a".to_string());
        app.handle_key(key(KeyCode::Enter), &visible, &[], true, false, &[]);
        assert!(app.expanded_quick_steps.is_empty());
    }

    #[test]
    fn errors_only_keeps_a_failed_leafs_ancestors_but_drops_an_unrelated_sibling() {
        let rows = [
            row("prime.guarded", 1, StatusKind::Done),
            row("prime.guarded.bad", 2, StatusKind::Failed),
            row("prime.ok", 1, StatusKind::Done),
        ];
        let mut app = App::new(false);
        app.errors_only = true;
        let visible = visible_rows(&rows, &app);
        let paths: Vec<&str> = visible.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["prime.guarded", "prime.guarded.bad"]);
    }

    #[test]
    fn slash_opens_filter_entry_and_enter_applies_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('/')), &visible, &[], true, false, &[]);
        assert!(matches!(app.dialog, Dialog::FilterInput(_)));
        app.handle_key(key(KeyCode::Char('a')), &visible, &[], true, false, &[]);
        app.handle_key(key(KeyCode::Enter), &visible, &[], true, false, &[]);
        assert_eq!(app.filter.as_deref(), Some("a"));
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn filter_drops_rows_that_do_not_match_and_have_no_matching_descendant() {
        let rows = [
            row("prime.alpha", 1, StatusKind::Done),
            row("prime.beta", 1, StatusKind::Done),
        ];
        let mut app = App::new(false);
        app.filter = Some("alp".to_string());
        let visible = visible_rows(&rows, &app);
        let paths: Vec<&str> = visible.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["prime.alpha"]);
    }

    #[test]
    fn c_opens_a_confirm_and_y_requests_a_cancel() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false, &[]);
        assert_eq!(action, Action::None);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: false }
        ));
        let action = app.handle_key(key(KeyCode::Char('y')), &visible, &[], true, false, &[]);
        assert_eq!(action, Action::Cancel);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn c_confirm_declined_with_n_does_nothing() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false, &[]);
        let action = app.handle_key(key(KeyCode::Char('n')), &visible, &[], true, false, &[]);
        assert_eq!(action, Action::None);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn q_while_running_asks_first_then_cancels_on_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, &[], true, false, &[]);
        assert_eq!(action, Action::None);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: true }
        ));
        let action = app.handle_key(key(KeyCode::Char('y')), &visible, &[], true, false, &[]);
        assert_eq!(action, Action::Cancel);
        assert!(app.quit_when_finished);
    }

    #[test]
    fn c_confirm_never_sets_quit_when_finished() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false, &[]);
        app.handle_key(key(KeyCode::Char('y')), &visible, &[], true, false, &[]);
        assert!(!app.quit_when_finished);
    }

    #[test]
    fn q_with_nothing_running_quits_immediately_with_no_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, &[], false, false, &[]);
        assert_eq!(action, Action::Quit);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn osi_watch_q_always_just_detaches_with_no_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(true);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, &[], true, false, &[]);
        assert_eq!(action, Action::Quit);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn ctrl_c_while_running_is_its_own_action_not_the_confirm_dialog() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(
            ctrl_key(KeyCode::Char('c')),
            &visible,
            &[],
            true,
            false,
            &[],
        );
        assert_eq!(action, Action::CtrlC);
        assert_eq!(app.dialog, Dialog::None, "Ctrl-C never opens a confirm");
    }

    #[test]
    fn ctrl_c_with_nothing_running_is_still_its_own_action() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(
            ctrl_key(KeyCode::Char('c')),
            &visible,
            &[],
            false,
            false,
            &[],
        );
        assert_eq!(action, Action::CtrlC);
    }

    #[test]
    fn ctrl_c_inside_an_open_dialog_is_never_merely_a_dismiss() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false, &[]);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: false }
        ));
        let action = app.handle_key(
            ctrl_key(KeyCode::Char('c')),
            &visible,
            &[],
            true,
            false,
            &[],
        );
        assert_eq!(action, Action::CtrlC);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn a_plain_c_with_no_control_modifier_is_unaffected_by_the_ctrl_c_check() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('/')), &visible, &[], false, false, &[]);
        let action = app.handle_key(key(KeyCode::Char('c')), &visible, &[], false, false, &[]);
        assert_eq!(action, Action::None);
        assert_eq!(app.dialog, Dialog::FilterInput("c".to_string()));
    }

    #[test]
    fn esc_returns_to_select_only_when_opened_from_select() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(true);
        app.opened_from_select = true;
        let action = app.handle_key(key(KeyCode::Esc), &visible, &[], false, false, &[]);
        assert_eq!(action, Action::BackToSelect);
    }

    #[test]
    fn esc_does_nothing_when_not_opened_from_select() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(true);
        let action = app.handle_key(key(KeyCode::Esc), &visible, &[], false, false, &[]);
        assert_eq!(action, Action::None);
    }

    #[test]
    fn question_mark_opens_help_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('?')), &visible, &[], false, false, &[]);
        assert_eq!(app.dialog, Dialog::Help);
        app.handle_key(key(KeyCode::Char('x')), &visible, &[], false, false, &[]);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn capital_t_cycles_the_theme_with_help_open_without_closing_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('?')), &visible, &[], false, false, &[]);
        assert_eq!(app.dialog, Dialog::Help);
        let action = app.handle_key(key(KeyCode::Char('T')), &visible, &[], false, false, &[]);
        assert_eq!(action, Action::CycleTheme);
        assert_eq!(
            app.dialog,
            Dialog::Help,
            "T must leave Help open, unlike every other key"
        );
    }

    #[test]
    fn v_opens_the_full_value_overlay_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, false, &[]);
        assert!(matches!(app.dialog, Dialog::FullValue(_)));
        app.handle_key(key(KeyCode::Esc), &visible, &[], false, false, &[]);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn v_opens_the_value_when_the_row_has_no_prompt_sent() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, false, &[]);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::Value));
    }

    #[test]
    fn v_opens_prompt_sent_first_when_the_row_has_one() {
        // Finding 5: a prompt leaf's own sent text is usually the
        // more useful of the two to read in full.
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true, &[]);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
    }

    #[test]
    fn pressing_v_again_while_open_cycles_the_field_instead_of_closing() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true, &[]);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true, &[]);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::Value));
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true, &[]);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
        // A key other than `v` still closes it, same as any overlay.
        app.handle_key(key(KeyCode::Esc), &visible, &[], false, true, &[]);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn flow_up_down_select_lanes_over_every_pass_in_plan_order() {
        let mut loop_row = row("prime.drafts", 1, StatusKind::Running);
        loop_row.kind = RowKind::Loop;
        let mut pass0 = row("prime.drafts.iter_0", 2, StatusKind::Running);
        pass0.kind = RowKind::Pass;
        let mut pass1 = row("prime.drafts.iter_1", 2, StatusKind::Running);
        pass1.kind = RowKind::Pass;
        let rows = [loop_row, pass0, pass1];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_rows = rows.to_vec();
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_0"));
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_1"));
        app.handle_key(key(KeyCode::Up), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_0"));
    }

    /// Issue #691's own follow-up: `↑↓` turns follow off once it
    /// moves the selection away from the pass the Flow screen's own
    /// Selected pane would otherwise default to (the running one),
    /// and turns it back on by itself the moment the selection lands
    /// back on that same pass -- the Tape's own land-back rule.
    #[test]
    fn flow_up_down_turns_follow_off_and_back_on_the_same_way_the_tape_does() {
        let mut loop_row = row("prime.drafts", 1, StatusKind::Running);
        loop_row.kind = RowKind::Loop;
        let mut pass0 = row("prime.drafts.iter_0", 2, StatusKind::Running);
        pass0.kind = RowKind::Pass;
        let mut pass1 = row("prime.drafts.iter_1", 2, StatusKind::Pending);
        pass1.kind = RowKind::Pass;
        let rows = [loop_row, pass0, pass1];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_rows = rows.to_vec();
        assert!(app.follow);
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_0"));
        assert!(app.follow, "landed on the running pass: follow stays on");
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_1"));
        assert!(!app.follow, "moved off the running pass: follow turns off");
        app.handle_key(key(KeyCode::Up), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_0"));
        assert!(
            app.follow,
            "landed back on the running pass: follow resumes"
        );
    }

    /// `j`/`k` select a lane on the Flow screen the same way `↑↓`
    /// do -- never the generic tree walk, which has no rail of its
    /// own to walk.
    #[test]
    fn flow_j_k_select_lanes_the_same_way_up_down_do() {
        let mut pass0 = row("prime.drafts.iter_0", 2, StatusKind::Running);
        pass0.kind = RowKind::Pass;
        let mut pass1 = row("prime.drafts.iter_1", 2, StatusKind::Running);
        pass1.kind = RowKind::Pass;
        let rows = [pass0, pass1];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_rows = rows.to_vec();
        app.handle_key(key(KeyCode::Char('j')), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_0"));
        app.handle_key(key(KeyCode::Char('j')), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_1"));
        app.handle_key(key(KeyCode::Char('k')), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.drafts.iter_0"));
    }

    /// PageUp/PageDown/Home/End/`g`/`G` have no tree to page or jump
    /// through on the Flow screen -- a no-op, leaving both the
    /// selection and follow untouched.
    #[test]
    fn flow_page_and_jump_keys_are_no_ops_and_never_touch_follow() {
        let mut pass0 = row("prime.drafts.iter_0", 2, StatusKind::Running);
        pass0.kind = RowKind::Pass;
        let rows = [pass0];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        for code in [
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Char('g'),
            KeyCode::Char('G'),
        ] {
            app.handle_key(key(code), &visible, &[], true, false, &[]);
            assert_eq!(app.selected_path, None, "{code:?}");
            assert!(app.follow, "{code:?}");
        }
    }

    #[test]
    fn flow_left_right_scroll_and_turn_follow_off() {
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        assert!(app.follow);
        app.handle_key(key(KeyCode::Right), &[], &[], true, false, &[]);
        assert!(!app.follow);
        assert_eq!(app.flow_scroll, 10);
        // Issue #704 rule 3: follow is already off, so the very next
        // ←→ moves the station selection instead of scrolling
        // further -- a no-op here with no station to move to
        // (`flow_lane_stations` empty), but the rail must not keep
        // scrolling either.
        app.handle_key(key(KeyCode::Left), &[], &[], true, false, &[]);
        assert_eq!(app.flow_scroll, 10);
    }

    /// Issue #704 rule 3: ←→ scrolls the rail only while actually
    /// following a live run -- with follow off, or the run ended, it
    /// instead moves the selection between the selected lane's own
    /// stations.
    #[test]
    fn flow_left_right_move_stations_once_follow_is_off_or_the_run_has_ended() {
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.follow = false;
        app.flow_lane_stations = vec![
            "prime.a".to_string(),
            "prime.b".to_string(),
            "prime.c".to_string(),
        ];
        app.handle_key(key(KeyCode::Right), &[], &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        assert_eq!(app.flow_scroll, 0, "the rail itself never scrolls here");
        app.handle_key(key(KeyCode::Right), &[], &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::Left), &[], &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));

        // Following, but the run itself has ended (`running` false):
        // still moves stations, never scrolls.
        app.follow = true;
        app.handle_key(key(KeyCode::Right), &[], &[], false, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        assert_eq!(app.flow_scroll, 0);
    }

    /// Issue #704 rule 4: `[`/`]` on the full-screen step move
    /// between effects in plan order, skipping a row that has not
    /// started (a container, or a row the run never reached) --
    /// `↑↓`/PgUp/PgDn/Home/End scroll its content instead, taking
    /// over before any screen's own bindings, and `esc` closes it.
    #[test]
    fn fullscreen_step_moves_between_started_effects_and_arrows_scroll_instead() {
        let mut app = App::new(false);
        app.tape_fullscreen_step = true;
        app.flow_rows = vec![
            row("prime", 0, StatusKind::Running),
            row("prime.a", 1, StatusKind::Done),
            row("prime.b", 1, StatusKind::Pending),
            row("prime.c", 1, StatusKind::Done),
        ];
        app.flow_rows[0].kind = oscilloscope_core::render::RowKind::Root;
        app.selected_path = Some("prime.a".to_string());

        app.handle_key(key(KeyCode::Char(']')), &[], &[], true, false, &[]);
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.c"),
            "prime.b hasn't started, so ] skips it"
        );
        app.handle_key(key(KeyCode::Char('[')), &[], &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));

        // ↑↓/PgUp/PgDn scroll the content, never the selection, while
        // the full-screen step is open.
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &[]);
        assert_eq!(app.details_scroll, 1);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::PageDown), &[], &[], true, false, &[]);
        assert_eq!(app.details_scroll, 1 + app.details_viewport.max(1));
        app.handle_key(key(KeyCode::Home), &[], &[], true, false, &[]);
        assert_eq!(app.details_scroll, 0);

        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &[]);
        assert!(!app.tape_fullscreen_step);
    }

    /// Issue #704 rule 4: `[`/`]` turns follow off, the same as
    /// every other way of moving the shared selection -- a live
    /// run's own next follow update would otherwise revert the step
    /// before it is ever seen.
    #[test]
    fn fullscreen_step_turns_follow_off() {
        let mut app = App::new(false);
        app.tape_fullscreen_step = true;
        app.flow_rows = vec![
            row("prime.a", 1, StatusKind::Done),
            row("prime.b", 1, StatusKind::Done),
        ];
        app.selected_path = Some("prime.a".to_string());
        assert!(app.follow);
        app.handle_key(key(KeyCode::Char(']')), &[], &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        assert!(!app.follow);
    }

    #[test]
    fn flow_plus_minus_open_and_fold_a_loops_own_lanes() {
        let mut loop_row = row("prime.drafts", 1, StatusKind::Running);
        loop_row.kind = RowKind::Loop;
        loop_row.loop_progress = Some(oscilloscope_core::render::LoopProgress {
            done: 0,
            total: Some(5),
            eta_s: None,
        });
        let rows = [loop_row];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_rows = rows.to_vec();
        app.selected_path = Some("prime.drafts".to_string());
        app.handle_key(key(KeyCode::Char('+')), &visible, &[], true, false, &[]);
        assert_eq!(app.flow_lanes_open.get("prime.drafts"), Some(&3));
        app.handle_key(key(KeyCode::Char('+')), &visible, &[], true, false, &[]);
        assert_eq!(app.flow_lanes_open.get("prime.drafts"), Some(&5));
        app.handle_key(key(KeyCode::Char('-')), &visible, &[], true, false, &[]);
        assert_eq!(app.flow_lanes_open.get("prime.drafts"), Some(&0));
    }

    /// Issue #691's own follow-up: `+`/`-` act on the fork that
    /// contains the current selection -- never a fallback to the
    /// first loop on the rail, which used to let them silently act
    /// on an unrelated fork whenever the selection wasn't a pass at
    /// all (nothing selected, or a selection outside every loop).
    #[test]
    fn flow_plus_minus_do_nothing_when_the_selection_is_outside_every_fork() {
        let mut loop_row = row("prime.drafts", 1, StatusKind::Running);
        loop_row.kind = RowKind::Loop;
        loop_row.loop_progress = Some(oscilloscope_core::render::LoopProgress {
            done: 0,
            total: Some(5),
            eta_s: None,
        });
        let other = row("prime.other", 1, StatusKind::Done);
        let rows = [loop_row, other];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_rows = rows.to_vec();
        app.selected_path = Some("prime.other".to_string());
        app.handle_key(key(KeyCode::Char('+')), &visible, &[], true, false, &[]);
        assert!(
            app.flow_lanes_open.is_empty(),
            "a selection outside every fork must not open the first loop's own lanes: {:?}",
            app.flow_lanes_open
        );
    }

    /// A loop whose body is a single nested container (an `if`, say)
    /// places that container's own entry directly at the pass's own
    /// path, with no separate `RowKind::Pass` row at all -- `↑↓` must
    /// still recognise it as a pass (`rail::is_pass_path`), not only
    /// a row literally of kind `Pass`.
    #[test]
    fn flow_up_down_recognises_a_pass_whose_body_is_a_single_nested_if() {
        let mut loop_row = row("prime.fan", 1, StatusKind::Running);
        loop_row.kind = RowKind::Loop;
        let mut pass0_if = row("prime.fan.iter_0", 2, StatusKind::Running);
        pass0_if.kind = RowKind::If;
        let mut pass1_if = row("prime.fan.iter_1", 2, StatusKind::Pending);
        pass1_if.kind = RowKind::If;
        let rows = [loop_row, pass0_if, pass1_if];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_rows = rows.to_vec();
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.fan.iter_0"));
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.fan.iter_1"));
    }

    #[test]
    fn flow_enter_toggles_fullscreen_the_same_as_tape_and_signal() {
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.flow_has_selection = true;
        assert!(!app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(app.tape_fullscreen_step);
    }

    /// Issue #691's own follow-up: `enter` on the Flow screen is a
    /// no-op with nothing actually selected, the same guard Signal's
    /// own `signal_has_step` already is -- never arming the toggle
    /// to spring open, unasked, once a selection finally appears.
    #[test]
    fn flow_enter_is_a_no_op_with_nothing_selected() {
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        assert!(!app.flow_has_selection);
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(!app.tape_fullscreen_step);
    }

    #[test]
    fn page_up_down_home_end_and_g_move_the_tree_selection_and_turn_off_follow() {
        let rows: Vec<Row> = (0..10)
            .map(|i| row(&format!("prime.r{i}"), 1, StatusKind::Done))
            .collect();
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.tree_viewport = 3;
        app.select_tree_path(Some("prime.r0".to_string()));
        app.handle_key(key(KeyCode::PageDown), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r3"));
        assert!(!app.follow);
        app.handle_key(key(KeyCode::PageUp), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r0"));
        app.handle_key(key(KeyCode::End), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r9"));
        app.handle_key(key(KeyCode::Char('g')), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r0"));
        app.handle_key(key(KeyCode::Char('G')), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r9"));
        app.handle_key(key(KeyCode::Home), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r0"));
    }

    #[test]
    fn selecting_a_tree_row_resets_the_details_scroll() {
        let rows = [
            row("prime.a", 1, StatusKind::Done),
            row("prime.b", 1, StatusKind::Done),
        ];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.select_tree_path(Some("prime.a".to_string()));
        app.details_scroll = 5;
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        assert_eq!(app.details_scroll, 0);
    }

    fn state_row(path: &str) -> StateRow {
        StateRow {
            path: path.to_string(),
            depth: 0,
            key: path.rsplit('.').next().unwrap().to_string(),
            has_children: false,
            status: RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
            dim: None,
            tokens: None,
        }
    }

    #[test]
    fn the_state_panes_own_selection_never_moves_the_orchestration_selection() {
        let tree_rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = tree_rows.iter().collect();
        let state_rows = [state_row("a"), state_row("b")];
        let state_visible: Vec<&StateRow> = state_rows.iter().collect();
        let mut app = App::new(false);
        app.pane = Pane::State;
        app.select_tree_path(Some("prime.a".to_string()));
        app.handle_key(
            key(KeyCode::Down),
            &visible,
            &state_visible,
            true,
            false,
            &[],
        );
        assert_eq!(app.state_selected_path.as_deref(), Some("a"));
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.a"),
            "the state pane's own selection must never move the orchestration one"
        );
        app.handle_key(
            key(KeyCode::Char('G')),
            &visible,
            &state_visible,
            true,
            false,
            &[],
        );
        assert_eq!(app.state_selected_path.as_deref(), Some("b"));
    }

    #[test]
    fn page_down_in_the_details_pane_scrolls_instead_of_moving_any_selection() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.pane = Pane::Details;
        app.details_viewport = 4;
        app.select_tree_path(Some("prime.a".to_string()));
        app.handle_key(key(KeyCode::PageDown), &visible, &[], true, false, &[]);
        assert_eq!(app.details_scroll, 4);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::Home), &visible, &[], true, false, &[]);
        assert_eq!(app.details_scroll, 0);
    }

    #[test]
    fn shift_t_returns_the_cycle_theme_action() {
        let mut app = App::new(false);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('T')), &[], &[], true, false, &[]),
            Action::CycleTheme
        );
    }

    #[test]
    fn shift_t_reaches_every_screen_and_the_full_screen_step() {
        use oscilloscope_core::screens::Screen;
        for screen in [Screen::Tape, Screen::Signal, Screen::Flow, Screen::Store] {
            let mut app = App::new(false);
            app.screen = screen;
            assert_eq!(
                app.handle_key(key(KeyCode::Char('T')), &[], &[], true, false, &[]),
                Action::CycleTheme,
                "T should cycle the theme on {screen:?}"
            );
        }
        let mut app = App::new(false);
        app.tape_fullscreen_step = true;
        assert_eq!(
            app.handle_key(key(KeyCode::Char('T')), &[], &[], true, false, &[]),
            Action::CycleTheme,
            "T should cycle the theme with the full-screen step open"
        );
    }

    #[test]
    fn esc_closes_the_help_overlay() {
        let mut app = App::new(false);
        app.dialog = Dialog::Help;
        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &[]);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn t_on_the_tape_toggles_the_tokens_view_instead_of_the_theme() {
        let mut app = App::new(false);
        app.screen = Screen::Tape;
        assert_eq!(
            app.handle_key(key(KeyCode::Char('t')), &[], &[], true, false, &[]),
            Action::None
        );
        assert!(app.tape_tokens_view);
        app.handle_key(key(KeyCode::Char('t')), &[], &[], true, false, &[]);
        assert!(!app.tape_tokens_view);
    }

    #[test]
    fn p_on_the_tape_toggles_previews() {
        let mut app = App::new(false);
        app.screen = Screen::Tape;
        assert!(app.tape_previews_on);
        app.handle_key(key(KeyCode::Char('p')), &[], &[], true, false, &[]);
        assert!(!app.tape_previews_on);
    }

    #[test]
    fn enter_on_the_tape_opens_the_selected_steps_fullscreen_view() {
        let mut app = App::new(false);
        app.screen = Screen::Tape;
        assert!(!app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &[]);
        assert!(!app.tape_fullscreen_step);
    }

    #[test]
    fn zero_and_two_switch_screens() {
        let mut app = App::new(false);
        assert_eq!(app.screen, Screen::Tape);
        app.handle_key(key(KeyCode::Char('2')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        app.handle_key(key(KeyCode::Char('0')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Tape);
    }

    #[test]
    fn one_switches_to_signal_and_three_switches_to_cost() {
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('2')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        app.handle_key(key(KeyCode::Char('1')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Signal);
        app.handle_key(key(KeyCode::Char('3')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Store);
    }

    #[test]
    fn switching_screens_keeps_selection_follow_folding_filters_and_collapsed_rows() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.select_tree_path(Some("prime.a".to_string()));
        app.follow = false;
        app.fold_enabled = false;
        app.filter = Some("needle".to_string());
        app.collapsed.insert("prime.a".to_string());
        app.handle_key(key(KeyCode::Char('2')), &visible, &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        assert!(!app.follow);
        assert!(!app.fold_enabled);
        assert_eq!(app.filter.as_deref(), Some("needle"));
        assert!(app.collapsed.contains("prime.a"));
    }

    fn state_row_with_children(path: &str, has_children: bool) -> StateRow {
        let mut row = state_row(path);
        row.has_children = has_children;
        row
    }

    #[test]
    fn left_right_collapse_and_expand_the_state_pane_independently_of_the_tree_pane() {
        let rows = [
            state_row_with_children("fan", true),
            state_row("fan.iter_0"),
        ];
        let state_rows: Vec<&StateRow> = rows.iter().collect();
        let mut app = App::new(false);
        app.pane = Pane::State;
        app.state_selected_path = Some("fan".to_string());
        app.handle_key(key(KeyCode::Left), &[], &state_rows, true, false, &[]);
        assert!(app.state_collapsed.contains("fan"));
        assert!(
            app.collapsed.is_empty(),
            "the orchestration pane's own collapsed set must be untouched"
        );
        let visible = visible_state_rows(&rows, &app);
        assert_eq!(
            visible.len(),
            1,
            "a collapsed row's own children are hidden"
        );
        app.handle_key(key(KeyCode::Right), &[], &state_rows, true, false, &[]);
        assert!(!app.state_collapsed.contains("fan"));
        let visible = visible_state_rows(&rows, &app);
        assert_eq!(visible.len(), 2);
    }

    fn store_app() -> App {
        let mut app = App::new(false);
        app.screen = Screen::Store;
        app
    }

    #[test]
    fn up_down_select_table_rows_in_order_and_clamp_at_the_ends() {
        let mut app = store_app();
        let rows = vec!["prime.a".to_string(), "prime.b".to_string()];
        // The screen's own cursor already shows `prime.a` (`store::
        // build_store`'s own fallback-to-first-row, issue #704 rule
        // 2) before any key at all, so the first ↓ moves past it.
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &rows);
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.b"),
            "clamped at the end"
        );
        app.handle_key(key(KeyCode::Up), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::Up), &[], &[], true, false, &rows);
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.a"),
            "clamped at the start"
        );
    }

    #[test]
    fn j_k_pgup_pgdn_home_end_g_gg_are_aliases_for_store_selection() {
        let mut app = store_app();
        let rows = vec![
            "prime.a".to_string(),
            "prime.b".to_string(),
            "prime.c".to_string(),
        ];
        // Same reasoning as the plain ↑↓ test above: the first `j`
        // moves past the screen's own already-shown first row.
        app.handle_key(key(KeyCode::Char('j')), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::Char('G')), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.c"));
        app.handle_key(key(KeyCode::Char('g')), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::End), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.c"));
        app.handle_key(key(KeyCode::Home), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::PageDown), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.c"));
        app.handle_key(key(KeyCode::Char('k')), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::PageUp), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
    }

    #[test]
    fn enter_only_opens_full_screen_with_a_row_selected() {
        let mut app = store_app();
        let rows: Vec<String> = Vec::new();
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &rows);
        assert!(
            !app.tape_fullscreen_step,
            "enter does nothing with no row selected"
        );
        let rows = vec!["prime.a".to_string()];
        app.selected_path = Some("prime.a".to_string());
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &rows);
        assert!(app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &rows);
        assert!(!app.tape_fullscreen_step);
    }

    /// Issue #704 rule 2: the selected effect has no tokens of its
    /// own, and no ancestor does either -- the table's own cursor
    /// draws on the first row anyway (`store::build_store`'s own
    /// fallback), and `enter` must open that same row, never do
    /// nothing just because `self.selected_path` itself isn't the
    /// table's first entry.
    #[test]
    fn enter_opens_the_first_row_even_when_the_selection_has_no_tokens_anywhere_in_its_lineage() {
        let mut app = store_app();
        let rows = vec!["prime.a".to_string(), "prime.b".to_string()];
        app.selected_path = Some("prime.other.leaf".to_string());
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &rows);
        assert!(app.tape_fullscreen_step);
    }

    #[test]
    fn switching_screens_resets_the_shared_fullscreen_toggle_for_the_store_too() {
        let mut app = store_app();
        app.selected_path = Some("prime.a".to_string());
        let rows = vec!["prime.a".to_string()];
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &rows);
        assert!(app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Char('0')), &[], &[], true, false, &rows);
        assert_eq!(app.screen, Screen::Tape);
        assert!(!app.tape_fullscreen_step);
    }

    #[test]
    fn store_keys_do_nothing_outside_the_store_screen() {
        let mut app = App::new(false);
        assert_eq!(app.screen, Screen::Tape);
        let rows = vec!["prime.a".to_string()];
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &rows);
        assert_eq!(app.selected_path, None);
    }

    #[test]
    fn i_o_dollar_tab_z_v_e_slash_and_arrows_do_nothing_on_the_store() {
        let mut app = store_app();
        let rows = vec!["prime.a".to_string()];
        for code in [
            KeyCode::Char('i'),
            KeyCode::Char('o'),
            KeyCode::Char('$'),
            KeyCode::Tab,
            KeyCode::Char('z'),
            KeyCode::Char('v'),
            KeyCode::Char('e'),
            KeyCode::Char('/'),
            KeyCode::Left,
            KeyCode::Right,
        ] {
            app.handle_key(key(code), &[], &[], true, false, &rows);
        }
        assert_eq!(app.selected_path, None);
        assert_eq!(app.pane, Pane::Tree);
        assert!(app.fold_enabled);
        assert!(!app.errors_only);
        assert_eq!(app.filter, None);
        assert_eq!(app.dialog, Dialog::None);
    }

    // -------------------------------------------------------------
    // Signal (issue #688)
    // -------------------------------------------------------------

    #[test]
    fn signal_zoom_in_and_out_step_through_the_five_levels_and_clamp_at_the_ends() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        assert_eq!(app.signal_column_ms, 100);
        app.signal_zoom_in();
        assert_eq!(app.signal_column_ms, 50);
        app.signal_zoom_in();
        assert_eq!(app.signal_column_ms, 50, "already the finest level");
        app.signal_zoom_out();
        app.signal_zoom_out();
        app.signal_zoom_out();
        app.signal_zoom_out();
        assert_eq!(app.signal_column_ms, 1000);
        app.signal_zoom_out();
        assert_eq!(app.signal_column_ms, 1000, "already the coarsest level");
    }

    #[test]
    fn plus_minus_zoom_the_signal_screen_and_are_no_ops_elsewhere() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.handle_key(key(KeyCode::Char('+')), &[], &[], true, false, &[]);
        assert_eq!(app.signal_column_ms, 50);
        app.handle_key(key(KeyCode::Char('-')), &[], &[], true, false, &[]);
        assert_eq!(app.signal_column_ms, 100);
        app.screen = Screen::Tape;
        app.handle_key(key(KeyCode::Char('+')), &[], &[], true, false, &[]);
        assert_eq!(
            app.signal_column_ms, 100,
            "+/- do nothing off the Signal screen"
        );
    }

    #[test]
    fn signal_scrub_moves_one_column_at_a_time_and_clamps_to_now_and_zero() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_now_col = Some(3);
        app.signal_scrub(-1);
        assert_eq!(
            app.signal_cursor_col,
            Some(2),
            "the first press off Now seeds from it"
        );
        app.signal_scrub(-1);
        app.signal_scrub(-1);
        assert_eq!(app.signal_cursor_col, Some(0));
        app.signal_scrub(-1);
        assert_eq!(app.signal_cursor_col, Some(0), "never below zero");
        app.signal_scrub(1);
        app.signal_scrub(1);
        app.signal_scrub(1);
        app.signal_scrub(1);
        assert_eq!(app.signal_cursor_col, Some(3), "never past Now");
    }

    /// #734: `signal_drawn_col` can hold a column
    /// the window has long since scrolled past (the shared tree
    /// selection's own, parked on a step that finished minutes ago)
    /// while following Now -- the first `←` must not teleport the
    /// window back to it, it must seed from Now instead.
    #[test]
    fn signal_scrub_ignores_a_drawn_col_outside_the_window() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_now_col = Some(21_000);
        app.signal_window_start = Some(15_750); // the window's own left edge
        app.signal_drawn_col = Some(10); // a stale selection, off screen
        app.signal_scrub(-1);
        assert_eq!(
            app.signal_cursor_col,
            Some(20_999),
            "the first press must seed from Now, not the off-screen column"
        );
    }

    /// The same first press, but the drawn column is still inside
    /// the window: it is honoured, as issue #704 rule 2 already
    /// relied on.
    #[test]
    fn signal_scrub_honours_a_drawn_col_inside_the_window() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_now_col = Some(21_000);
        app.signal_window_start = Some(15_750);
        app.signal_drawn_col = Some(16_000); // inside the window
        app.signal_scrub(-1);
        assert_eq!(app.signal_cursor_col, Some(15_999));
    }

    #[test]
    fn n_returns_the_signal_cursor_to_now() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_now_col = Some(5);
        app.signal_scrub(-2);
        assert_eq!(app.signal_cursor_col, Some(3));
        app.handle_key(key(KeyCode::Char('n')), &[], &[], true, false, &[]);
        assert_eq!(app.signal_cursor_col, None);
    }

    #[test]
    fn left_right_scrub_the_signal_screen_and_move_the_tree_selection_elsewhere() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_now_col = Some(2);
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false, &[]);
        assert_eq!(app.signal_cursor_col, Some(1));
        app.screen = Screen::Tape;
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false, &[]);
        assert_eq!(
            app.signal_cursor_col,
            Some(1),
            "← on another screen never touches the Signal's own cursor"
        );
    }

    /// Issue #704 rule 2: a scrub's own column must give way to a
    /// selection made through any other screen -- moving the Tape's
    /// own selection (which, unlike the no-op above, actually goes
    /// through `select_tree_path`) drops the stale scrub, so
    /// switching back to the Signal reads the new selection's own
    /// column instead of wherever the cursor was left.
    #[test]
    fn a_selection_made_elsewhere_drops_the_signals_own_stale_scrub() {
        let rows = [
            row("prime.a", 1, StatusKind::Done),
            row("prime.b", 1, StatusKind::Done),
        ];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_now_col = Some(5);
        app.signal_scrub(-2);
        assert_eq!(app.signal_cursor_col, Some(3));
        app.screen = Screen::Tape;
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false, &[]);
        assert_eq!(
            app.signal_cursor_col, None,
            "a real selection elsewhere drops the stale scrub"
        );
    }

    #[test]
    fn enter_opens_and_closes_the_signal_fullscreen_step_the_same_as_the_tapes() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_has_step = true;
        assert!(!app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(app.tape_fullscreen_step);
        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &[]);
        assert!(!app.tape_fullscreen_step);
    }

    #[test]
    fn enter_on_signal_is_a_no_op_with_no_step_under_the_cursor() {
        let mut app = App::new(false);
        app.screen = Screen::Signal;
        app.signal_has_step = false;
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &[]);
        assert!(
            !app.tape_fullscreen_step,
            "nothing to open yet, so the toggle must not arm itself for later"
        );
    }

    #[test]
    fn switching_screens_resets_the_shared_fullscreen_step_toggle() {
        let mut app = App::new(false);
        app.screen = Screen::Tape;
        app.tape_fullscreen_step = true;
        app.handle_key(key(KeyCode::Char('1')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Signal);
        assert!(
            !app.tape_fullscreen_step,
            "the Tape's own open step view must not reappear as Signal's"
        );
    }

    /// Issue #704 rule 4: a scroll left over from one screen's own
    /// detail pane must not show up, mid-scroll, in an unrelated
    /// pane the next screen draws.
    #[test]
    fn switching_screens_resets_the_shared_details_scroll() {
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.details_scroll = 7;
        app.handle_key(key(KeyCode::Char('0')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Tape);
        assert_eq!(app.details_scroll, 0);
    }

    fn select_key(code: KeyCode) -> (KeyCode, KeyModifiers) {
        (code, KeyModifiers::NONE)
    }

    #[test]
    fn select_cursor_clamps_at_both_ends() {
        let mut state = SelectState::new(1);
        let (code, modifiers) = select_key(KeyCode::Up);
        state.handle_key(code, modifiers, false, 2, false);
        assert_eq!(state.cursor, 0);
        let (code, modifiers) = select_key(KeyCode::Up);
        state.handle_key(code, modifiers, false, 2, false);
        assert_eq!(state.cursor, 0, "cursor must not go below 0");
        let (code, modifiers) = select_key(KeyCode::End);
        state.handle_key(code, modifiers, false, 2, false);
        assert_eq!(state.cursor, 2);
        let (code, modifiers) = select_key(KeyCode::Down);
        state.handle_key(code, modifiers, false, 2, false);
        assert_eq!(state.cursor, 2, "cursor must not go past max_index");
    }

    #[test]
    fn select_enter_on_all_opens_the_all_screen() {
        let mut state = SelectState::new(0);
        let (code, modifiers) = select_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, false, 2, false),
            SelectKeyOutcome::OpenAll
        );
    }

    #[test]
    fn select_enter_on_a_run_row_attaches_it() {
        let mut state = SelectState::new(1);
        let (code, modifiers) = select_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, false, 2, false),
            SelectKeyOutcome::AttachRun(0)
        );
    }

    #[test]
    fn select_enter_on_an_open_recent_row_attaches_it() {
        let mut state = SelectState::new(0);
        let (code, modifiers) = select_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, true, 0, true),
            SelectKeyOutcome::AttachRecent(0)
        );
    }

    #[test]
    fn select_enter_on_a_recent_row_that_cannot_be_opened_is_inert() {
        let mut state = SelectState::new(0);
        let (code, modifiers) = select_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, true, 0, false),
            SelectKeyOutcome::None
        );
    }

    #[test]
    fn select_q_quits_at_once_with_exit_0() {
        let mut state = SelectState::new(1);
        let (code, modifiers) = select_key(KeyCode::Char('q'));
        assert_eq!(
            state.handle_key(code, modifiers, false, 2, false),
            SelectKeyOutcome::Quit(0)
        );
    }

    #[test]
    fn select_ctrl_c_quits_with_exit_130_even_with_help_open() {
        let mut state = SelectState::new(1);
        state.help_open = true;
        assert_eq!(
            state.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL, false, 2, false),
            SelectKeyOutcome::Quit(130)
        );
        assert!(!state.help_open, "Ctrl-C must not merely dismiss Help");
    }

    #[test]
    fn select_capital_t_cycles_the_theme_even_with_help_open() {
        let mut state = SelectState::new(1);
        state.help_open = true;
        let (code, modifiers) = select_key(KeyCode::Char('T'));
        assert_eq!(
            state.handle_key(code, modifiers, false, 2, false),
            SelectKeyOutcome::CycleTheme
        );
        assert!(
            state.help_open,
            "T must leave Help open, unlike every other key"
        );
    }

    #[test]
    fn select_a_plain_c_and_number_keys_do_nothing() {
        let mut state = SelectState::new(1);
        for code in [
            KeyCode::Char('c'),
            KeyCode::Char('0'),
            KeyCode::Char('1'),
            KeyCode::Char('2'),
            KeyCode::Char('3'),
        ] {
            assert_eq!(
                state.handle_key(code, KeyModifiers::NONE, false, 2, false),
                SelectKeyOutcome::None
            );
        }
        assert_eq!(
            state.cursor, 1,
            "none of these keys may move the cursor either"
        );
    }

    #[test]
    fn select_any_other_key_closes_help_without_acting_on_it() {
        let mut state = SelectState::new(1);
        state.help_open = true;
        assert_eq!(
            state.handle_key(KeyCode::Enter, KeyModifiers::NONE, false, 2, false),
            SelectKeyOutcome::None
        );
        assert!(!state.help_open);
        assert_eq!(
            state.cursor, 1,
            "the key that closed Help must not also act"
        );
    }

    fn all_key(code: KeyCode) -> (KeyCode, KeyModifiers) {
        (code, KeyModifiers::NONE)
    }

    #[test]
    fn all_0_to_3_set_the_shared_screen() {
        let mut state = AllState::new();
        for (ch, expected) in [
            ('1', oscilloscope_core::screens::Screen::Signal),
            ('2', oscilloscope_core::screens::Screen::Flow),
            ('3', oscilloscope_core::screens::Screen::Store),
            ('0', oscilloscope_core::screens::Screen::Tape),
        ] {
            let (code, modifiers) = all_key(KeyCode::Char(ch));
            state.handle_key(code, modifiers, 3, 6, 1);
            assert_eq!(state.screen, expected);
        }
    }

    #[test]
    fn all_right_wraps_within_the_row() {
        let mut state = AllState::new();
        state.focus = 2; // last column of a 3-wide row
        let (code, modifiers) = all_key(KeyCode::Right);
        state.handle_key(code, modifiers, 3, 6, 1);
        assert_eq!(state.focus, 0, "wraps to the first column of the same row");
    }

    #[test]
    fn all_left_wraps_within_the_row() {
        let mut state = AllState::new();
        state.focus = 0;
        let (code, modifiers) = all_key(KeyCode::Left);
        state.handle_key(code, modifiers, 3, 6, 1);
        assert_eq!(state.focus, 2, "wraps to the last column of the same row");
    }

    #[test]
    fn all_down_moves_a_full_row_and_wraps() {
        let mut state = AllState::new();
        state.focus = 1; // row 0, col 1
        let (code, modifiers) = all_key(KeyCode::Down);
        state.handle_key(code, modifiers, 3, 6, 1);
        assert_eq!(state.focus, 4, "row 1, same column");
        state.handle_key(code, modifiers, 3, 6, 1);
        assert_eq!(state.focus, 1, "wraps back to row 0");
    }

    #[test]
    fn all_up_moves_a_full_row_and_wraps() {
        let mut state = AllState::new();
        state.focus = 1; // row 0, col 1
        let (code, modifiers) = all_key(KeyCode::Up);
        state.handle_key(code, modifiers, 3, 6, 1);
        assert_eq!(state.focus, 4, "wraps up to the last row, same column");
    }

    #[test]
    fn all_enter_opens_the_focused_tile() {
        let mut state = AllState::new();
        state.focus = 2;
        let (code, modifiers) = all_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, 3, 6, 1),
            AllKeyOutcome::Open(2)
        );
    }

    #[test]
    fn all_enter_does_nothing_with_no_tiles_on_the_page() {
        let mut state = AllState::new();
        let (code, modifiers) = all_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, 3, 0, 1),
            AllKeyOutcome::None
        );
    }

    #[test]
    fn all_esc_goes_back_to_select() {
        let mut state = AllState::new();
        let (code, modifiers) = all_key(KeyCode::Esc);
        assert_eq!(
            state.handle_key(code, modifiers, 3, 6, 1),
            AllKeyOutcome::BackToSelect
        );
    }

    #[test]
    fn all_q_quits_at_once_with_no_confirm() {
        let mut state = AllState::new();
        let (code, modifiers) = all_key(KeyCode::Char('q'));
        assert_eq!(
            state.handle_key(code, modifiers, 3, 6, 1),
            AllKeyOutcome::Quit(0)
        );
    }

    #[test]
    fn all_u_opens_the_combined_signal() {
        let mut state = AllState::new();
        let (code, modifiers) = all_key(KeyCode::Char('u'));
        assert_eq!(
            state.handle_key(code, modifiers, 3, 6, 1),
            AllKeyOutcome::Combined
        );
    }

    #[test]
    fn combined_right_scrubs_from_now_and_left_scrubs_back() {
        let mut state = CombinedState::new();
        let (code, modifiers) = all_key(KeyCode::Right);
        state.handle_key(code, modifiers, 6, 27, 150);
        assert_eq!(state.scrub_col, Some(28));
        let (code, modifiers) = all_key(KeyCode::Left);
        state.handle_key(code, modifiers, 6, 27, 150);
        assert_eq!(state.scrub_col, Some(27));
    }

    #[test]
    fn combined_scrub_never_passes_max_col() {
        let mut state = CombinedState::new();
        state.scrub_col = Some(150);
        let (code, modifiers) = all_key(KeyCode::Right);
        state.handle_key(code, modifiers, 6, 27, 150);
        assert_eq!(state.scrub_col, Some(150));
    }

    #[test]
    fn combined_n_returns_to_now() {
        let mut state = CombinedState::new();
        state.scrub_col = Some(10);
        let (code, modifiers) = all_key(KeyCode::Char('n'));
        state.handle_key(code, modifiers, 6, 27, 150);
        assert_eq!(state.scrub_col, None);
    }

    #[test]
    fn combined_up_down_move_the_cursor_and_wrap() {
        let mut state = CombinedState::new();
        let (code, modifiers) = all_key(KeyCode::Up);
        state.handle_key(code, modifiers, 6, 27, 150);
        assert_eq!(
            state.cursor_run, 5,
            "up from the first run wraps to the last"
        );
        let (code, modifiers) = all_key(KeyCode::Down);
        state.handle_key(code, modifiers, 6, 27, 150);
        assert_eq!(
            state.cursor_run, 0,
            "down from the last run wraps to the first"
        );
    }

    #[test]
    fn combined_enter_opens_the_selected_run() {
        let mut state = CombinedState::new();
        state.cursor_run = 2;
        let (code, modifiers) = all_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, 6, 27, 150),
            CombinedKeyOutcome::Open(2)
        );
    }

    #[test]
    fn combined_esc_goes_back_to_the_tiles() {
        let mut state = CombinedState::new();
        let (code, modifiers) = all_key(KeyCode::Esc);
        assert_eq!(
            state.handle_key(code, modifiers, 6, 27, 150),
            CombinedKeyOutcome::BackToTiles
        );
    }

    #[test]
    fn combined_q_quits_at_once() {
        let mut state = CombinedState::new();
        let (code, modifiers) = all_key(KeyCode::Char('q'));
        assert_eq!(
            state.handle_key(code, modifiers, 6, 27, 150),
            CombinedKeyOutcome::Quit(0)
        );
    }

    #[test]
    fn all_page_down_and_up_turn_the_page_and_reset_focus() {
        let mut state = AllState::new();
        state.focus = 4;
        let (code, modifiers) = all_key(KeyCode::PageDown);
        state.handle_key(code, modifiers, 3, 6, 3);
        assert_eq!(state.page, 1);
        assert_eq!(state.focus, 0);
        let (code, modifiers) = all_key(KeyCode::PageUp);
        state.handle_key(code, modifiers, 3, 6, 3);
        assert_eq!(state.page, 0);
        let (code, modifiers) = all_key(KeyCode::PageUp);
        state.handle_key(code, modifiers, 3, 6, 3);
        assert_eq!(state.page, 2, "wraps to the last page");
    }

    #[test]
    fn all_ctrl_c_quits_with_exit_130_even_with_help_open() {
        let mut state = AllState::new();
        state.help_open = true;
        assert_eq!(
            state.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL, 3, 6, 1),
            AllKeyOutcome::Quit(130)
        );
        assert!(!state.help_open);
    }

    #[test]
    fn all_any_key_closes_help_without_acting_on_it() {
        let mut state = AllState::new();
        state.help_open = true;
        let (code, modifiers) = all_key(KeyCode::Enter);
        assert_eq!(
            state.handle_key(code, modifiers, 3, 6, 1),
            AllKeyOutcome::None
        );
        assert!(!state.help_open);
    }

    #[test]
    fn all_capital_t_cycles_the_theme_even_with_help_open() {
        let mut state = AllState::new();
        state.help_open = true;
        let (code, modifiers) = all_key(KeyCode::Char('T'));
        assert_eq!(
            state.handle_key(code, modifiers, 3, 6, 1),
            AllKeyOutcome::CycleTheme
        );
        assert!(
            state.help_open,
            "T must leave Help open, unlike every other key"
        );
    }

    #[test]
    fn combined_capital_t_cycles_the_theme_even_with_help_open() {
        let mut state = CombinedState::new();
        state.help_open = true;
        let (code, modifiers) = all_key(KeyCode::Char('T'));
        assert_eq!(
            state.handle_key(code, modifiers, 6, 27, 150),
            CombinedKeyOutcome::CycleTheme
        );
        assert!(
            state.help_open,
            "T must leave Help open, unlike every other key"
        );
    }
}
