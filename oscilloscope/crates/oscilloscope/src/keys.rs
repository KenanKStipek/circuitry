//! `App`: the TUI's own navigation/dialog state and key handling
//! (DESIGN.md §6.3's key table, issue #434). Pure and
//! terminal-independent beyond `crossterm::event::KeyEvent` itself —
//! every test here builds its own `Row`s and feeds in key events
//! directly, with no real terminal involved.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oscilloscope_core::render::{FullValueField, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Details,
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
}

/// The TUI's whole navigation/dialog state (DESIGN.md §6.3). `running`
/// is the caller's own signal of whether the engine osp started (or,
/// for `osp watch`, the watched run) is still going — it decides
/// whether `q` asks first at all.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub selected_path: Option<String>,
    pub collapsed: std::collections::HashSet<String>,
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
}

impl App {
    pub fn new(is_watch: bool) -> Self {
        App {
            selected_path: None,
            collapsed: std::collections::HashSet::new(),
            pane: Pane::Tree,
            follow: false,
            errors_only: false,
            filter: None,
            dialog: Dialog::None,
            is_watch,
            quit_when_finished: false,
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
            Dialog::None => self.handle_key_normal(key, rows, running, has_prompt_sent),
        }
    }

    fn handle_key_normal(
        &mut self,
        key: KeyEvent,
        rows: &[&Row],
        running: bool,
        has_prompt_sent: bool,
    ) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(rows, -1);
                self.follow = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(rows, 1);
                self.follow = false;
            }
            KeyCode::Left => {
                if let Some(path) = &self.selected_path {
                    self.collapsed.insert(path.clone());
                }
            }
            KeyCode::Right => {
                if let Some(path) = &self.selected_path {
                    self.collapsed.remove(path);
                }
            }
            KeyCode::Tab => {
                self.pane = match self.pane {
                    Pane::Tree => Pane::Details,
                    Pane::Details => Pane::Tree,
                };
            }
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

    fn move_selection(&mut self, rows: &[&Row], delta: i32) {
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
        self.selected_path = Some(rows[next].path.clone());
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
        app.handle_key(key(KeyCode::Down), &visible, true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
        app.handle_key(key(KeyCode::Down), &visible, true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        // Already at the end: another Down must not go out of bounds.
        app.handle_key(key(KeyCode::Down), &visible, true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
        app.handle_key(key(KeyCode::Up), &visible, true, false);
        assert_eq!(app.selected_path.as_deref(), Some("prime.a"));
    }

    #[test]
    fn left_collapses_the_selected_row_and_right_expands_it_again() {
        let rows = [row("prime.loop", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.selected_path = Some("prime.loop".to_string());
        app.handle_key(key(KeyCode::Left), &visible, true, false);
        assert!(app.collapsed.contains("prime.loop"));
        app.handle_key(key(KeyCode::Right), &visible, true, false);
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
    fn pressing_f_toggles_follow() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        assert!(!app.follow);
        app.handle_key(key(KeyCode::Char('f')), &visible, true, false);
        assert!(app.follow);
        app.handle_key(key(KeyCode::Char('f')), &visible, true, false);
        assert!(!app.follow);
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
        app.handle_key(key(KeyCode::Char('/')), &visible, true, false);
        assert!(matches!(app.dialog, Dialog::FilterInput(_)));
        app.handle_key(key(KeyCode::Char('a')), &visible, true, false);
        app.handle_key(key(KeyCode::Enter), &visible, true, false);
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
        let action = app.handle_key(key(KeyCode::Char('c')), &visible, true, false);
        assert_eq!(action, Action::None);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: false }
        ));
        let action = app.handle_key(key(KeyCode::Char('y')), &visible, true, false);
        assert_eq!(action, Action::Cancel);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn c_confirm_declined_with_n_does_nothing() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, true, false);
        let action = app.handle_key(key(KeyCode::Char('n')), &visible, true, false);
        assert_eq!(action, Action::None);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn q_while_running_asks_first_then_cancels_on_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, true, false);
        assert_eq!(action, Action::None);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: true }
        ));
        let action = app.handle_key(key(KeyCode::Char('y')), &visible, true, false);
        assert_eq!(action, Action::Cancel);
        assert!(app.quit_when_finished);
    }

    #[test]
    fn c_confirm_never_sets_quit_when_finished() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, true, false);
        app.handle_key(key(KeyCode::Char('y')), &visible, true, false);
        assert!(!app.quit_when_finished);
    }

    #[test]
    fn q_with_nothing_running_quits_immediately_with_no_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, false, false);
        assert_eq!(action, Action::Quit);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn osp_watch_q_always_just_detaches_with_no_confirm() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(true);
        let action = app.handle_key(key(KeyCode::Char('q')), &visible, true, false);
        assert_eq!(action, Action::Quit);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn ctrl_c_while_running_is_its_own_action_not_the_confirm_dialog() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(ctrl_key(KeyCode::Char('c')), &visible, true, false);
        assert_eq!(action, Action::CtrlC);
        assert_eq!(app.dialog, Dialog::None, "Ctrl-C never opens a confirm");
    }

    #[test]
    fn ctrl_c_with_nothing_running_is_still_its_own_action() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        let action = app.handle_key(ctrl_key(KeyCode::Char('c')), &visible, false, false);
        assert_eq!(action, Action::CtrlC);
    }

    #[test]
    fn ctrl_c_inside_an_open_dialog_is_never_merely_a_dismiss() {
        let rows = [row("prime.a", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('c')), &visible, true, false);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmCancel { quit_after: false }
        ));
        let action = app.handle_key(ctrl_key(KeyCode::Char('c')), &visible, true, false);
        assert_eq!(action, Action::CtrlC);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn a_plain_c_with_no_control_modifier_is_unaffected_by_the_ctrl_c_check() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('/')), &visible, false, false);
        let action = app.handle_key(key(KeyCode::Char('c')), &visible, false, false);
        assert_eq!(action, Action::None);
        assert_eq!(app.dialog, Dialog::FilterInput("c".to_string()));
    }

    #[test]
    fn question_mark_opens_help_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('?')), &visible, false, false);
        assert_eq!(app.dialog, Dialog::Help);
        app.handle_key(key(KeyCode::Char('x')), &visible, false, false);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn v_opens_the_full_value_overlay_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, false, false);
        assert!(matches!(app.dialog, Dialog::FullValue(_)));
        app.handle_key(key(KeyCode::Esc), &visible, false, false);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn v_opens_the_value_when_the_row_has_no_prompt_sent() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, false, false);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::Value));
    }

    #[test]
    fn v_opens_prompt_sent_first_when_the_row_has_one() {
        // Finding 5: a prompt leaf's own sent text is usually the
        // more useful of the two to read in full.
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
    }

    #[test]
    fn pressing_v_again_while_open_cycles_the_field_instead_of_closing() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('v')), &visible, false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
        app.handle_key(key(KeyCode::Char('v')), &visible, false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::Value));
        app.handle_key(key(KeyCode::Char('v')), &visible, false, true);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
        // A key other than `v` still closes it, same as any overlay.
        app.handle_key(key(KeyCode::Esc), &visible, false, true);
        assert_eq!(app.dialog, Dialog::None);
    }

    #[test]
    fn tab_switches_pane() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        assert_eq!(app.pane, Pane::Tree);
        app.handle_key(key(KeyCode::Tab), &visible, false, false);
        assert_eq!(app.pane, Pane::Details);
    }
}
