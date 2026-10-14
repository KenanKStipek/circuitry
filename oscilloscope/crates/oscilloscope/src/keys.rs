//! `App`: the TUI's own navigation/dialog state and key handling
//! (DESIGN.md §6.3's key table, issue #434). Pure and
//! terminal-independent beyond `crossterm::event::KeyEvent` itself —
//! every test here builds its own `Row`s and feeds in key events
//! directly, with no real terminal involved.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oscilloscope_core::render::{FullValueField, Row, StateRow};

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
    /// to cancel while `osp watch`'s own `quit_after` has nothing to
    /// wait on cancelling at all (watch owns no engine to cancel —
    /// `q` there always just detaches, §6.3).
    Quit,
    /// Ctrl-C (review finding K3), checked before any dialog and
    /// never merely dismissing one: the main loop forwards SIGINT at
    /// once with no confirm while something's running (a second
    /// Ctrl-C starts the same 10s kill deadline a real repeated
    /// SIGINT would), quits immediately with nothing running, and in
    /// `osp watch` always just detaches — the same three cases a real
    /// forwarded SIGINT already covers in plain mode.
    CtrlC,
    /// `t` (issue #535): cycle to the next colour theme, wrapping.
    CycleTheme,
}

/// The TUI's whole navigation/dialog state (DESIGN.md §6.3). `running`
/// is the caller's own signal of whether the engine osp started (or,
/// for `osp watch`, the watched run) is still going — it decides
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
    /// `osp watch`'s own flavour of quitting (§6.3): `q` never needs a
    /// confirm there, since watch owns no engine of its own to cancel
    /// — it only ever detaches.
    pub is_watch: bool,
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
            quit_when_finished: false,
            tree_scroll: 0,
            state_scroll: 0,
            details_scroll: 0,
            tree_viewport: 0,
            state_viewport: 0,
            details_viewport: 0,
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
        }
    }

    /// `rows` is the already-visible list this frame drew (collapsed
    /// subtrees and the errors-only/filter views already applied,
    /// `visible_rows` below) — navigation and collapse/expand act on
    /// exactly what the user can see, in the order they see it.
    /// `has_prompt_sent` is whether the currently selected row's own
    /// details carry a `prompt_sent` (review finding 5): `v` opens
    /// that first when there is one, since it's usually the more
    /// useful of the two to read in full, and the value otherwise.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        rows: &[&Row],
        state_rows: &[&StateRow],
        running: bool,
        has_prompt_sent: bool,
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
            Dialog::None => self.handle_key_normal(key, rows, state_rows, running, has_prompt_sent),
        }
    }

    fn handle_key_normal(
        &mut self,
        key: KeyEvent,
        rows: &[&Row],
        state_rows: &[&StateRow],
        running: bool,
        has_prompt_sent: bool,
    ) -> Action {
        match key.code {
            // DESIGN.md §6.3/issue #527: every one of these moves (or
            // scrolls) whichever pane currently has focus — the
            // orchestration pane's own selection when `Pane::Tree`,
            // turning follow off (a manual move always does); the
            // state pane's own, independent selection when
            // `Pane::State`; the details pane's own scroll position
            // when `Pane::Details`.
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
            KeyCode::Left if self.pane == Pane::Tree => {
                if let Some(path) = &self.selected_path {
                    self.collapsed.insert(path.clone());
                }
            }
            KeyCode::Right if self.pane == Pane::Tree => {
                if let Some(path) = &self.selected_path {
                    self.collapsed.remove(path);
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
            KeyCode::Tab => {
                // Finding 12: on a small terminal the state pane isn't
                // drawn at all (`tui::draw` zeroes `state_viewport` the
                // same frame) — `tab` must not focus a pane with nothing
                // to show or scroll.
                self.pane = match self.pane {
                    Pane::Tree => Pane::Details,
                    Pane::Details if self.state_viewport > 0 => Pane::State,
                    Pane::Details | Pane::State => Pane::Tree,
                };
            }
            KeyCode::Char('t') => return Action::CycleTheme,
            KeyCode::Char('f') => self.follow = !self.follow,
            KeyCode::Char('e') => self.errors_only = !self.errors_only,
            KeyCode::Char('/') => {
                self.dialog = Dialog::FilterInput(self.filter.clone().unwrap_or_default());
            }
            KeyCode::Char('v') => {
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

    fn move_or_scroll(&mut self, rows: &[&Row], state_rows: &[&StateRow], delta: i32) {
        match self.pane {
            Pane::Tree => {
                self.move_tree_selection(rows, delta);
                self.follow = false;
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
                    self.state_selected_path = Some(target.path.clone());
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
        self.state_selected_path = Some(rows[next].path.clone());
    }

    /// `f`'s own target (§6.3): the innermost row osp considers
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

fn matches_or_has_match(rows: &[Row], row: &Row, pred: &dyn Fn(&Row) -> bool) -> bool {
    if pred(row) {
        return true;
    }
    let prefix = format!("{}.", row.path);
    rows.iter().any(|r| r.path.starts_with(&prefix) && pred(r))
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
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        // Already at the end: another Down must not go out of bounds.
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::Up), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
    }

    #[test]
    fn left_collapses_the_selected_row_and_right_expands_it_again() {
        let rows = [row("prime.loop", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.selected_path = Some("prime.loop".to_string());
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false);
        assert!(app.collapsed.contains("prime.loop"));
        app.handle_key(key(KeyCode::Right), &visible, &[], true, false);
        assert!(!app.collapsed.contains("prime.loop"));
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

    #[test]
    fn follow_is_on_by_default_and_f_toggles_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        assert!(app.follow, "issue #527: follow is on when the TUI starts");
        app.handle_key(key(KeyCode::Char('f')), &visible, &[], true, false);
        assert!(!app.follow);
        app.handle_key(key(KeyCode::Char('f')), &visible, &[], true, false);
        assert!(app.follow);
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
        app.handle_key(key(KeyCode::Char('/')), &visible, &[], true, false);
        assert!(matches!(app.dialog, Dialog::FilterInput(_)));
        app.handle_key(key(KeyCode::Char('a')), &visible, &[], true, false);
        app.handle_key(key(KeyCode::Enter), &visible, &[], true, false);
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
        let action = app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false);
        assert_eq!(action, Action::None);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: false }
        ));
        let action = app.handle_key(key(KeyCode::Char('y')), &visible, &[], true, false);
        assert_eq!(action, Action::Cancel);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn c_confirm_declined_with_n_does_nothing() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false);
        let action = app.handle_key(key(KeyCode::Char('n')), &visible, &[], true, false);
        assert_eq!(action, Action::None);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn q_while_running_asks_first_then_cancels_on_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, &[], true, false);
        assert_eq!(action, Action::None);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: true }
        ));
        let action = app.handle_key(key(KeyCode::Char('y')), &visible, &[], true, false);
        assert_eq!(action, Action::Cancel);
        assert!(app.quit_when_finished);
    }

    #[test]
    fn c_confirm_never_sets_quit_when_finished() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false);
        app.handle_key(key(KeyCode::Char('y')), &visible, &[], true, false);
        assert!(!app.quit_when_finished);
    }

    #[test]
    fn q_with_nothing_running_quits_immediately_with_no_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, &[], false, false);
        assert_eq!(action, Action::Quit);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn osp_watch_q_always_just_detaches_with_no_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(true);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, &[], true, false);
        assert_eq!(action, Action::Quit);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn ctrl_c_while_running_is_its_own_action_not_the_confirm_dialog() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(ctrl_key(KeyCode::Char('c')), &visible, &[], true, false);
        assert_eq!(action, Action::CtrlC);
        assert_eq!(app.dialog, Dialog::None, "Ctrl-C never opens a confirm");
    }

    #[test]
    fn ctrl_c_with_nothing_running_is_still_its_own_action() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(ctrl_key(KeyCode::Char('c')), &visible, &[], false, false);
        assert_eq!(action, Action::CtrlC);
    }

    #[test]
    fn ctrl_c_inside_an_open_dialog_is_never_merely_a_dismiss() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, &[], true, false);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: false }
        ));
        let action = app.handle_key(ctrl_key(KeyCode::Char('c')), &visible, &[], true, false);
        assert_eq!(action, Action::CtrlC);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn a_plain_c_with_no_control_modifier_is_unaffected_by_the_ctrl_c_check() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('/')), &visible, &[], false, false);
        let action = app.handle_key(key(KeyCode::Char('c')), &visible, &[], false, false);
        assert_eq!(action, Action::None);
        assert_eq!(app.dialog, Dialog::FilterInput("c".to_string()));
    }

    #[test]
    fn question_mark_opens_help_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('?')), &visible, &[], false, false);
        assert_eq!(app.dialog, Dialog::Help);
        app.handle_key(key(KeyCode::Char('x')), &visible, &[], false, false);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn v_opens_the_full_value_overlay_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, false);
        assert!(matches!(app.dialog, Dialog::FullValue(_)));
        app.handle_key(key(KeyCode::Esc), &visible, &[], false, false);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn v_opens_the_value_when_the_row_has_no_prompt_sent() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, false);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::Value));
    }

    #[test]
    fn v_opens_prompt_sent_first_when_the_row_has_one() {
        // Finding 5: a prompt leaf's own sent text is usually the
        // more useful of the two to read in full.
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
    }

    #[test]
    fn pressing_v_again_while_open_cycles_the_field_instead_of_closing() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::Value));
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
        // A key other than `v` still closes it, same as any overlay.
        app.handle_key(key(KeyCode::Esc), &visible, &[], false, true);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn tab_cycles_tree_details_state_and_back() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.state_viewport = 8;
        assert_eq!(app.pane, Pane::Tree);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false);
        assert_eq!(app.pane, Pane::Details);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false);
        assert_eq!(app.pane, Pane::State);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false);
        assert_eq!(app.pane, Pane::Tree);
    }

    /// Finding 12: a small terminal drops the state pane entirely
    /// (`tui::draw` zeroes `state_viewport` that same frame) — `tab`
    /// must skip straight from details back to the orchestration pane.
    #[test]
    fn tab_skips_the_state_pane_when_it_is_not_drawn() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.state_viewport = 0;
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false);
        assert_eq!(app.pane, Pane::Details);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false);
        assert_eq!(app.pane, Pane::Tree, "the state pane has nothing to focus");
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
        app.handle_key(key(KeyCode::PageDown), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r3"));
        assert!(!app.follow);
        app.handle_key(key(KeyCode::PageUp), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r0"));
        app.handle_key(key(KeyCode::End), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r9"));
        app.handle_key(key(KeyCode::Char('g')), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r0"));
        app.handle_key(key(KeyCode::Char('G')), &visible, &[], true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.r9"));
        app.handle_key(key(KeyCode::Home), &visible, &[], true, false);
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
        app.handle_key(key(KeyCode::Down), &visible, &[], true, false);
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
        app.handle_key(key(KeyCode::Down), &visible, &state_visible, true, false);
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
        app.handle_key(key(KeyCode::PageDown), &visible, &[], true, false);
        assert_eq!(app.details_scroll, 4);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::Home), &visible, &[], true, false);
        assert_eq!(app.details_scroll, 0);
    }
}
