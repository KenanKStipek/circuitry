//! `App`: the TUI's own navigation/dialog state and key handling
//! (DESIGN.md §6.3's key table, issue #434). Pure and
//! terminal-independent beyond `crossterm::event::KeyEvent` itself —
//! every test here builds its own `Row`s and feeds in key events
//! directly, with no real terminal involved.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oscilloscope_core::render::{FullValueField, Row, StateRow};
use oscilloscope_core::screens::Screen;
use oscilloscope_core::treemap::{Measure, TreemapNode};

/// DESIGN.md §6.3's `tab` cycle, issue #527: `Tree` → `Details` →
/// `State` → `Tree`. The treemap and token bar have no pane of their
/// own to focus — neither scrolls or takes a selection independent of
/// the orchestration pane's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Details,
    State,
    /// The Flow screen's own story pane (issue #635) -- scrollable,
    /// reached from `Details` by `tab` only while `screen == Flow`;
    /// every other screen's own cycle never visits it at all.
    Story,
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
    /// Which screen is on display (issue #626): `0`/`2` switch
    /// here, `1`/`3` do nothing (`Screen::from_key` already refuses
    /// them). Selection, follow, folding, filters and collapsed rows
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
    /// The Cost screen's own treemap measure (issue #632): `i`/`o`/
    /// `$` switch it, the same key again returns to `Total`.
    pub cost_measure: oscilloscope_core::treemap::Measure,
    /// The Cost screen's own zoom stack (issue #632): each `⏎`
    /// pushes the selected store's own path, zooming into its
    /// children; `esc` pops one level back out. Empty means the
    /// top-level tiles (the root's own children).
    pub cost_zoom: Vec<String>,
    /// The selected store's own path at the Cost screen's *current*
    /// zoom level -- independent of `selected_path`/
    /// `state_selected_path`, same reasoning as those two staying
    /// independent of each other.
    pub cost_selected: Option<String>,
    /// The Flow screen's own story pane scroll position and interior
    /// height (issue #635), the same pairing `tree_scroll`/
    /// `tree_viewport` already are for the orchestration pane.
    pub story_scroll: usize,
    pub story_viewport: usize,
    /// `s` (issue #635): the Flow screen's selected pane shows today's
    /// state tree instead of the focused step's own detail, so a
    /// named report stays selectable (#610). `s` again swaps back.
    pub flow_selected_shows_state: bool,
    /// `l` (issue #635): the Flow screen's story pane shows the full
    /// log instead. `l` again swaps back.
    pub flow_story_shows_log: bool,
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
            fold_enabled: true,
            expanded_quick_steps: std::collections::BTreeSet::new(),
            details_source: Pane::Tree,
            screen: Screen::default(),
            cost_measure: oscilloscope_core::treemap::Measure::Total,
            cost_zoom: Vec::new(),
            cost_selected: None,
            story_scroll: 0,
            story_viewport: 0,
            flow_selected_shows_state: false,
            flow_story_shows_log: false,
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
    /// `cost_tree` is the Cost screen's own by-store tree for the
    /// currently selected measure (issue #632) -- empty for every
    /// caller that doesn't care about it (every screen but Cost never
    /// reads it at all). The tree is one frame stale at worst, the
    /// same way `rows`/`state_rows` already are.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        rows: &[&Row],
        state_rows: &[&StateRow],
        running: bool,
        has_prompt_sent: bool,
        cost_tree: &[TreemapNode],
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
            Dialog::None => {
                self.handle_key_normal(key, rows, state_rows, running, has_prompt_sent, cost_tree)
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
        cost_tree: &[TreemapNode],
    ) -> Action {
        // Issue #632: the Cost screen's own keys (`i`/`o`/`$`, ↑↓ to
        // select a store in size order, `⏎`/`esc` to zoom) take over
        // first, before any of the generic panes' own bindings below
        // -- `0`-`3` (screen switching), `t`, `?`, `c`/`q` and Ctrl-C
        // still fall through to the match below unchanged.
        if self.screen == Screen::Cost && self.handle_cost_key(key, cost_tree) {
            return Action::None;
        }
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
                if let Some(path) = self.selected_path.clone() {
                    self.collapsed.insert(path.clone());
                    // Issue #613: the same key re-folds a quick-steps
                    // group ⏎ had expanded in place, from any of its
                    // steps, not only its own representative
                    // (`expanded_quick_steps`' only key): scan backward
                    // over same-depth rows from the selected one until
                    // the representative's own row is found. The scan
                    // stops at the first row that is neither itself a
                    // quick step (`fold::is_quick_step`'s own test) nor
                    // an unexpanded quick-steps summary row, so an
                    // unrelated sibling between two groups -- or
                    // before the selected row -- never gets scanned
                    // past into an earlier group's expansion.
                    if let Some(selected_idx) = rows.iter().position(|r| r.path == path) {
                        let depth = rows[selected_idx].depth;
                        for row in rows[..=selected_idx].iter().rev() {
                            let in_group = oscilloscope_core::fold::is_quick_step(row)
                                || row.kind == oscilloscope_core::render::RowKind::QuickSteps;
                            if row.depth != depth || !in_group {
                                break;
                            }
                            if self.expanded_quick_steps.remove(&row.path) {
                                break;
                            }
                        }
                    }
                }
            }
            KeyCode::Right if self.pane == Pane::Tree => {
                if let Some(path) = &self.selected_path {
                    self.collapsed.remove(path);
                }
            }
            // Issue #626: ⏎ on the overview opens screen 2 with the
            // running step selected -- the same deepest-running-row
            // tie-break `follow_target` and the now pane's own focus
            // use, over the rows actually visible right now (root
            // excluded: it's "running" for the whole run, never a
            // real step to jump to).
            KeyCode::Enter if self.screen == Screen::Overview => {
                if let Some(target) = rows
                    .iter()
                    .filter(|r| {
                        r.status.kind == oscilloscope_core::model::StatusKind::Running
                            && r.kind != oscilloscope_core::render::RowKind::Root
                    })
                    .max_by_key(|r| r.depth)
                {
                    let path = target.path.clone();
                    self.screen = Screen::Flow;
                    self.select_tree_path(Some(path));
                }
            }
            // Issue #613: ⏎ on a quick-steps summary row expands it
            // in place -- recognised from the row's own `RowKind`,
            // not by parsing its label.
            KeyCode::Enter if self.pane == Pane::Tree => {
                if let Some(path) = &self.selected_path {
                    if rows.iter().any(|r| {
                        r.path == *path && r.kind == oscilloscope_core::render::RowKind::QuickSteps
                    }) {
                        self.expanded_quick_steps.insert(path.clone());
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
            KeyCode::Tab => {
                // Issue #635: the Flow screen's own cycle is flow →
                // selected → story → flow. `s` swaps the selected
                // pane's own *content* between the focused step and
                // today's state tree, but the selected slot in this
                // cycle must follow that swap too (#610) -- otherwise
                // a state-tree report `s` just swapped in can never
                // actually get focus, or its own `←`/`→`/`v`.
                self.pane = if self.screen == Screen::Flow {
                    match self.pane {
                        Pane::Tree => {
                            if self.flow_selected_shows_state {
                                Pane::State
                            } else {
                                Pane::Details
                            }
                        }
                        // Finding 12's own rule, same reason: the
                        // story pane isn't drawn at all below the
                        // three-pane layout, so `tab` must not land
                        // on it with nothing to show or scroll.
                        Pane::Details | Pane::State if self.story_viewport > 0 => Pane::Story,
                        Pane::Details | Pane::State | Pane::Story => Pane::Tree,
                    }
                } else {
                    // Finding 12: on a small terminal the state pane
                    // isn't drawn at all (`tui::draw` zeroes
                    // `state_viewport` the same frame) — `tab` must not
                    // focus a pane with nothing to show or scroll.
                    match self.pane {
                        Pane::Tree => Pane::Details,
                        Pane::Details if self.state_viewport > 0 => Pane::State,
                        Pane::Details | Pane::State | Pane::Story => Pane::Tree,
                    }
                };
            }
            // Issue #626: `0`/`2` switch screens; `1`/`3` do
            // nothing yet (`Screen::from_key` already refuses them,
            // same as any other key with no binding). Selection,
            // follow, folding, filters and collapsed rows are all
            // outside `App::screen`, so none of them reset here.
            KeyCode::Char(c @ '0'..='3') => {
                if let Some(screen) = Screen::from_key(c) {
                    self.screen = screen;
                }
            }
            KeyCode::Char('t') => return Action::CycleTheme,
            // Issue #635: the Flow screen's own content swaps -- `s`
            // for the selected pane (the focused step's own detail vs
            // today's state tree, so a named report stays
            // selectable), `l` for the story pane (the story vs the
            // full log). Both no-ops on any other screen.
            KeyCode::Char('s') if self.screen == Screen::Flow => {
                self.flow_selected_shows_state = !self.flow_selected_shows_state;
            }
            KeyCode::Char('l') if self.screen == Screen::Flow => {
                self.flow_story_shows_log = !self.flow_story_shows_log;
            }
            KeyCode::Char('f') => self.follow = !self.follow,
            // Issue #613: `z` turns folding off to show the full,
            // unfolded tree, and back on again.
            KeyCode::Char('z') => self.fold_enabled = !self.fold_enabled,
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
            Pane::Story => self.story_viewport.max(1),
        }
    }

    /// The Cost screen's own keys (issue #632), tried before any of
    /// the generic panes' own bindings; `true` means the key was
    /// consumed here and `handle_key_normal`'s own match below must
    /// not also act on it.
    fn handle_cost_key(&mut self, key: KeyEvent, tree: &[TreemapNode]) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cost_selection(tree, -1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cost_selection(tree, 1);
                true
            }
            // `⏎` zooms into the selected store (DESIGN.md) -- only
            // once it actually has children of its own to zoom into;
            // a leaf (or a store too small to show its own children
            // at all) has nothing further down to show.
            KeyCode::Enter => {
                let level = self.cost_level(tree);
                let items = oscilloscope_core::treemap::sorted_children(level);
                let target = self
                    .cost_selected
                    .as_deref()
                    .and_then(|p| items.iter().find(|n| n.path == p).copied())
                    .or_else(|| items.first().copied());
                if let Some(node) = target {
                    if !node.children.is_empty() {
                        self.cost_zoom.push(node.path.clone());
                        self.cost_selected = None;
                    }
                }
                true
            }
            // `esc` zooms back out one level (DESIGN.md) -- a no-op
            // at the top level, same as every other screen's `esc`
            // with nothing open.
            KeyCode::Esc => {
                if !self.cost_zoom.is_empty() {
                    self.cost_zoom.pop();
                    self.cost_selected = None;
                }
                true
            }
            // `i`/`o`/`$` switch the treemap's own measure; the same
            // key again returns to the default, total tokens
            // (`Measure::from_key`'s own toggle rule).
            KeyCode::Char(c @ ('i' | 'o' | '$')) => {
                if let Some(measure) = Measure::from_key(self.cost_measure, c) {
                    self.cost_measure = measure;
                }
                true
            }
            _ => false,
        }
    }

    /// The Cost screen's own current zoom level's children (issue
    /// #632): walks `self.cost_zoom`'s own stack of store paths into
    /// `tree`, dropping any trailing entry that no longer resolves
    /// (the underlying state changed since it was pushed) rather than
    /// leaving the screen stuck on a level that no longer exists.
    pub(crate) fn cost_level<'a>(&mut self, tree: &'a [TreemapNode]) -> &'a [TreemapNode] {
        let mut level = tree;
        let mut valid_depth = 0;
        for path in &self.cost_zoom {
            match level.iter().find(|n| &n.path == path) {
                Some(node) => {
                    level = &node.children;
                    valid_depth += 1;
                }
                None => break,
            }
        }
        self.cost_zoom.truncate(valid_depth);
        level
    }

    /// `↑`/`↓` select tiles in size order (DESIGN.md), the same
    /// largest-total-first, tie-by-key order `treemap::layout` itself
    /// lays tiles out in -- so moving the selection one step always
    /// matches moving to the tile actually next to it on screen.
    fn move_cost_selection(&mut self, tree: &[TreemapNode], delta: i32) {
        let level = self.cost_level(tree);
        let items = oscilloscope_core::treemap::sorted_children(level);
        if items.is_empty() {
            return;
        }
        let current = self
            .cost_selected
            .as_deref()
            .and_then(|p| items.iter().position(|n| n.path == p));
        let next = match current {
            Some(idx) => (idx as i32 + delta).clamp(0, items.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => items.len() - 1,
        };
        self.cost_selected = Some(items[next].path.clone());
    }

    fn move_or_scroll(&mut self, rows: &[&Row], state_rows: &[&StateRow], delta: i32) {
        match self.pane {
            Pane::Tree => {
                self.move_tree_selection(rows, delta);
                self.follow = false;
            }
            Pane::State => self.move_state_selection(state_rows, delta),
            Pane::Details => self.scroll_details(delta),
            Pane::Story => self.scroll_story(delta),
        }
    }

    /// The story pane's own scroll (issue #635: "the selected pane
    /// and the story scroll with ↑↓ when they have focus") -- clamped
    /// by `tui::draw`'s own render the same way `scroll_details`'s
    /// `details_scroll` already is, since only it knows the story's
    /// real line count this frame.
    fn scroll_story(&mut self, delta: i32) {
        if delta < 0 {
            self.story_scroll = self.story_scroll.saturating_sub((-delta) as usize);
        } else {
            self.story_scroll = self.story_scroll.saturating_add(delta as usize);
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
            Pane::Story => {
                self.story_scroll = if to_first { 0 } else { usize::MAX };
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

    /// Keeps selection and `collapsed` meaningful once folding has
    /// changed what `rows` actually draws (issue #613): if the
    /// selected path (or a `collapsed` entry) is no longer among
    /// `rows` at all -- its own container folded into a breadcrumb
    /// mid-run -- this re-points it to the visible row whose path is
    /// that one, or extends it with `.` (the breadcrumb that
    /// absorbed it).
    pub fn reconcile_folding(&mut self, rows: &[Row]) {
        if let Some(selected) = self.selected_path.clone() {
            if !rows.iter().any(|r| r.path == selected) {
                if let Some(absorbed) = Self::absorbing_row_path(rows, &selected) {
                    self.select_tree_path_keeping_details(Some(absorbed));
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
    fn left_collapses_the_selected_row_and_right_expands_it_again() {
        let rows = [row("prime.loop", 1, StatusKind::Running)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.selected_path = Some("prime.loop".to_string());
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false, &[]);
        assert!(app.collapsed.contains("prime.loop"));
        app.handle_key(key(KeyCode::Right), &visible, &[], true, false, &[]);
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
        app.handle_key(key(KeyCode::Char('f')), &visible, &[], true, false, &[]);
        assert!(!app.follow);
        app.handle_key(key(KeyCode::Char('f')), &visible, &[], true, false, &[]);
        assert!(app.follow);
    }

    #[test]
    fn fold_is_on_by_default_and_z_toggles_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        assert!(app.fold_enabled, "issue #613: fold is on by default");
        app.handle_key(key(KeyCode::Char('z')), &visible, &[], true, false, &[]);
        assert!(!app.fold_enabled);
        app.handle_key(key(KeyCode::Char('z')), &visible, &[], true, false, &[]);
        assert!(app.fold_enabled);
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

    #[test]
    fn enter_on_the_overview_opens_screen_2_with_the_running_step_selected() {
        let rows = [
            row("prime", 0, StatusKind::Running),
            row("prime.a", 1, StatusKind::Done),
            row("prime.b", 1, StatusKind::Running),
        ];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        assert_eq!(
            app.screen,
            Screen::Overview,
            "the overview is the default screen"
        );
        app.handle_key(key(KeyCode::Enter), &visible, &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        assert_eq!(app.selected_path.as_deref(), Some("prime.b"));
    }

    #[test]
    fn enter_on_the_overview_with_nothing_running_does_nothing() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Enter), &visible, &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Overview);
    }

    #[test]
    fn enter_expands_a_quick_steps_row_and_left_folds_it_again() {
        let mut quick_steps_row = row("prime.a", 1, StatusKind::Done);
        quick_steps_row.label = "3 quick steps".to_string();
        quick_steps_row.kind = RowKind::QuickSteps;
        let rows = [quick_steps_row];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        // Quick steps are a screen 2 concept -- on the overview
        // (the default screen), ⏎ opens screen 2 instead (issue
        // #626).
        app.screen = Screen::Flow;
        app.selected_path = Some("prime.a".to_string());
        app.handle_key(key(KeyCode::Enter), &visible, &[], true, false, &[]);
        assert!(app.expanded_quick_steps.contains("prime.a"));
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false, &[]);
        assert!(!app.expanded_quick_steps.contains("prime.a"));
    }

    /// ← re-folds an expanded group from any of its steps, not only
    /// from its own representative (the first one, and the only path
    /// `expanded_quick_steps` ever stores) -- selecting the *second*
    /// exploded step and pressing ← must still re-fold the whole
    /// group.
    #[test]
    fn left_re_folds_an_expanded_quick_steps_group_from_any_of_its_steps() {
        let quick = |path: &str| {
            let mut r = row(path, 1, StatusKind::Done);
            r.duration_s = Some(0.3);
            r
        };
        let rows = [quick("prime.a"), quick("prime.b"), quick("prime.c")];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.expanded_quick_steps.insert("prime.a".to_string());
        app.selected_path = Some("prime.b".to_string());
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false, &[]);
        assert!(!app.expanded_quick_steps.contains("prime.a"));
    }

    /// The backward scan stops at the first row that isn't itself a
    /// quick step, so an unrelated sibling between two groups -- or
    /// after one -- never gets scanned past into an earlier group's
    /// own expansion.
    #[test]
    fn left_on_an_unrelated_sibling_leaves_an_earlier_groups_expansion_alone() {
        let mut slow = row("prime.slow", 1, StatusKind::Done);
        slow.duration_s = Some(5.0);
        let rows = [
            row("prime.q1", 1, StatusKind::Done),
            row("prime.q2", 1, StatusKind::Done),
            slow,
            row("prime.box", 1, StatusKind::Done),
        ];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.expanded_quick_steps.insert("prime.q1".to_string());
        app.selected_path = Some("prime.box".to_string());
        app.handle_key(key(KeyCode::Left), &visible, &[], true, false, &[]);
        assert!(app.expanded_quick_steps.contains("prime.q1"));
        assert!(app.collapsed.contains("prime.box"));
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
    fn v_opens_the_full_value_overlay_and_any_key_closes_it() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
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
        app.handle_key(key(KeyCode::Char('v')), &visible, &[], false, true, &[]);
        assert_eq!(app.dialog, Dialog::FullValue(FullValueField::PromptSent));
    }

    #[test]
    fn pressing_v_again_while_open_cycles_the_field_instead_of_closing() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
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
    fn the_flow_screens_tab_cycle_visits_the_story_pane_not_the_state_pane() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.state_viewport = 8;
        app.story_viewport = 8;
        assert_eq!(app.pane, Pane::Tree);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
        assert_eq!(app.pane, Pane::Details);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
        assert_eq!(app.pane, Pane::Story);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
        assert_eq!(app.pane, Pane::Tree);
    }

    #[test]
    fn after_s_tab_reaches_the_state_pane_and_arrow_keys_move_its_selection() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let state_rows = [state_row("a"), state_row("b")];
        let state_visible: Vec<&StateRow> = state_rows.iter().collect();
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.state_viewport = 8;
        app.story_viewport = 8;
        app.handle_key(
            key(KeyCode::Char('s')),
            &visible,
            &state_visible,
            false,
            false,
            &[],
        );
        assert!(app.flow_selected_shows_state);
        app.handle_key(
            key(KeyCode::Tab),
            &visible,
            &state_visible,
            false,
            false,
            &[],
        );
        assert_eq!(app.pane, Pane::State);
        app.handle_key(
            key(KeyCode::Down),
            &visible,
            &state_visible,
            false,
            false,
            &[],
        );
        assert_eq!(app.state_selected_path.as_deref(), Some("a"));
        assert_eq!(app.details_source, Pane::State);
    }

    #[test]
    fn s_and_l_toggle_the_flow_screens_own_content_swaps_only_on_the_flow_screen() {
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('s')), &[], &[], false, false, &[]);
        assert!(!app.flow_selected_shows_state, "not the Flow screen yet");
        app.screen = Screen::Flow;
        app.handle_key(key(KeyCode::Char('s')), &[], &[], false, false, &[]);
        assert!(app.flow_selected_shows_state);
        app.handle_key(key(KeyCode::Char('s')), &[], &[], false, false, &[]);
        assert!(!app.flow_selected_shows_state);
        app.handle_key(key(KeyCode::Char('l')), &[], &[], false, false, &[]);
        assert!(app.flow_story_shows_log);
        app.handle_key(key(KeyCode::Char('l')), &[], &[], false, false, &[]);
        assert!(!app.flow_story_shows_log);
    }

    #[test]
    fn up_down_scroll_the_story_pane_when_it_has_focus() {
        let mut app = App::new(false);
        app.screen = Screen::Flow;
        app.pane = Pane::Story;
        app.handle_key(key(KeyCode::Down), &[], &[], false, false, &[]);
        assert_eq!(app.story_scroll, 1);
        app.handle_key(key(KeyCode::Up), &[], &[], false, false, &[]);
        assert_eq!(app.story_scroll, 0);
    }

    #[test]
    fn tab_cycles_tree_details_state_and_back() {
        let rows = [row("prime.a", 1, StatusKind::Done)];
        let visible: Vec<&Row> = rows.iter().collect();
        let mut app = App::new(false);
        app.state_viewport = 8;
        assert_eq!(app.pane, Pane::Tree);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
        assert_eq!(app.pane, Pane::Details);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
        assert_eq!(app.pane, Pane::State);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
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
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
        assert_eq!(app.pane, Pane::Details);
        app.handle_key(key(KeyCode::Tab), &visible, &[], false, false, &[]);
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
    fn t_returns_the_cycle_theme_action() {
        let mut app = App::new(false);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('t')), &[], &[], true, false, &[]),
            Action::CycleTheme
        );
    }

    #[test]
    fn zero_and_two_switch_screens() {
        let mut app = App::new(false);
        assert_eq!(app.screen, Screen::Overview);
        app.handle_key(key(KeyCode::Char('2')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        app.handle_key(key(KeyCode::Char('0')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Overview);
    }

    #[test]
    fn one_does_nothing_but_three_switches_to_cost() {
        let mut app = App::new(false);
        app.handle_key(key(KeyCode::Char('2')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        app.handle_key(key(KeyCode::Char('1')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Flow);
        app.handle_key(key(KeyCode::Char('3')), &[], &[], true, false, &[]);
        assert_eq!(app.screen, Screen::Cost);
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

    fn cost_leaf(key: &str, total: u64) -> TreemapNode {
        TreemapNode::leaf(key, total, Some(oscilloscope_core::model::StatusKind::Done))
    }

    fn cost_app() -> App {
        let mut app = App::new(false);
        app.screen = Screen::Cost;
        app
    }

    #[test]
    fn i_o_dollar_switch_the_cost_measure_and_the_same_key_returns_to_total() {
        let mut app = cost_app();
        assert_eq!(app.cost_measure, Measure::Total);
        app.handle_key(key(KeyCode::Char('i')), &[], &[], true, false, &[]);
        assert_eq!(app.cost_measure, Measure::In);
        app.handle_key(key(KeyCode::Char('i')), &[], &[], true, false, &[]);
        assert_eq!(app.cost_measure, Measure::Total);
        app.handle_key(key(KeyCode::Char('o')), &[], &[], true, false, &[]);
        assert_eq!(app.cost_measure, Measure::Out);
        app.handle_key(key(KeyCode::Char('$')), &[], &[], true, false, &[]);
        assert_eq!(app.cost_measure, Measure::Cost);
        app.handle_key(key(KeyCode::Char('$')), &[], &[], true, false, &[]);
        assert_eq!(app.cost_measure, Measure::Total);
    }

    #[test]
    fn up_down_select_cost_tiles_in_size_order() {
        let mut app = cost_app();
        let tree = vec![cost_leaf("small", 10), cost_leaf("big", 100)];
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        assert_eq!(app.cost_selected.as_deref(), Some("big"));
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        assert_eq!(app.cost_selected.as_deref(), Some("small"));
        app.handle_key(key(KeyCode::Up), &[], &[], true, false, &tree);
        assert_eq!(app.cost_selected.as_deref(), Some("big"));
    }

    #[test]
    fn enter_zooms_into_the_selected_store_and_esc_zooms_back_out() {
        let mut app = cost_app();
        let tree = vec![TreemapNode {
            key: "review".to_string(),
            path: "prime.review".to_string(),
            total: 100,
            weight: 100,
            status: Some(oscilloscope_core::model::StatusKind::Done),
            children: vec![cost_leaf("agent", 60), cost_leaf("fix", 40)],
        }];
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        assert_eq!(app.cost_selected.as_deref(), Some("prime.review"));
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &tree);
        assert_eq!(app.cost_zoom, vec!["prime.review".to_string()]);
        // Zoomed in: selection now moves among `review`'s own children.
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        assert_eq!(app.cost_selected.as_deref(), Some("agent"));
        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &tree);
        assert!(app.cost_zoom.is_empty());
    }

    #[test]
    fn enter_does_nothing_on_a_leaf_with_no_children() {
        let mut app = cost_app();
        let tree = vec![cost_leaf("agent", 60)];
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        app.handle_key(key(KeyCode::Enter), &[], &[], true, false, &tree);
        assert!(app.cost_zoom.is_empty(), "a leaf has nothing to zoom into");
    }

    #[test]
    fn esc_at_the_top_level_is_a_no_op() {
        let mut app = cost_app();
        let tree = vec![cost_leaf("agent", 60)];
        app.handle_key(key(KeyCode::Esc), &[], &[], true, false, &tree);
        assert!(app.cost_zoom.is_empty());
    }

    #[test]
    fn a_stale_zoom_path_that_no_longer_resolves_is_dropped() {
        let mut app = cost_app();
        app.cost_zoom = vec!["prime.gone".to_string()];
        let tree = vec![cost_leaf("agent", 60)];
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        assert!(
            app.cost_zoom.is_empty(),
            "a zoom path the current tree no longer has must be dropped, not leave the screen stuck"
        );
        assert_eq!(app.cost_selected.as_deref(), Some("agent"));
    }

    #[test]
    fn cost_keys_do_nothing_outside_the_cost_screen() {
        let mut app = App::new(false);
        assert_eq!(app.screen, Screen::Overview);
        let tree = vec![cost_leaf("agent", 60)];
        app.handle_key(key(KeyCode::Char('i')), &[], &[], true, false, &tree);
        assert_eq!(app.cost_measure, Measure::Total);
        app.handle_key(key(KeyCode::Down), &[], &[], true, false, &tree);
        assert_eq!(app.cost_selected, None);
    }
}
