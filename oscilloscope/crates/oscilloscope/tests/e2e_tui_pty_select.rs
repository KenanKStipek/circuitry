//! End-to-end tests for bare `osi`'s own Select screen (issue #698,
//! design v1 part 5) against a real `cof`, on a real pseudo-terminal
//! -- gated the same way `e2e_tui_pty.rs` is: `OSI_E2E_COF=1` and
//! `cof` on `PATH`, a temporary `HOME`, stripped credentials, a hard
//! per-test timeout.
//!
//! Every test here spawns its own `osi` through `spawn_osi_on_pty`,
//! which sets `OSI_PROCESS_SCAN=0` -- Select itself never reads
//! `discover_runs`'s own machine-wide `ps` scan in this file, only
//! the run registry. (One test, `bare_osi_shows_the_empty_select_
//! with_a_recorded_run`, still runs a real `cof` through
//! `osi_command`, unaffected by that flag, but to completion, before
//! Select ever opens -- never concurrently with it.) [`SELECT_LOCK`]
//! still serialises every test in this file against every other
//! (`cargo test` runs a single binary's own tests concurrently by
//! default): kept as a defence against the kind of cross-test
//! leftover-process flakiness [`AutoKill`]'s own doc comment
//! describes, now that no test here depends on a live scan to find
//! one.

mod support;

use std::path::Path;
use std::process::Child;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use support::{
    SCRIPTED_CONFIG, SELECT_SLOW_DOC, SIGNAL_DELAY, TestHome, assert_no_leftover_process,
    drain_continuously, e2e_enabled, launch_osi_log, open_pty, osi_command, scripted_config,
    send_bytes, spawn_osi_on_pty, wait_for_registry_entries, wait_for_screen_text,
    wait_for_step_start, wait_with_timeout, wait_with_timeout_capturing_stdout, write_doc,
};

/// Serialises every test in this file against every other (see the
/// module's own doc comment) -- held for a whole test's own
/// duration, not around any one step of it, since the leftover
/// process [`AutoKill`]'s own doc comment describes could appear at
/// any point during a concurrently-running test.
static SELECT_LOCK: Mutex<()> = Mutex::new(());

/// Ends its own child on drop, including while unwinding from a
/// failed assertion (this crate's own test profile doesn't set
/// `panic = "abort"`): every background `osi`/`cof` this file spawns
/// is wrapped in one from the moment it's spawned, so a panic
/// partway through a test can never leave it running underneath a
/// *later* test in this same file, once [`SELECT_LOCK`] is released
/// -- exactly how this file's own early flakiness traced back to a
/// stray, still-running `sleep` left behind by an earlier panic, long
/// before this guard existed. [`Self::into_inner`] hands the child
/// back, bare, for a caller that means to wait on it normally; a
/// `Child` taken that way is on its own again, same as it always
/// was, which is exactly the point right before this file's own
/// explicit, final `wait_with_timeout`.
struct AutoKill(Option<Child>);

impl AutoKill {
    fn new(child: Child) -> Self {
        AutoKill(Some(child))
    }

    fn into_inner(mut self) -> Child {
        self.0.take().expect("AutoKill still holds its child")
    }
}

impl Drop for AutoKill {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A quick, allow-listed document (an `echo` tool) that finishes at
/// once -- used to populate history for the empty Select's own
/// Recent Runs table, where [`SELECT_SLOW_DOC`] (which never
/// finishes inside a test's own timeout) would not do.
const QUICK_DOC: &str = "effects:\n  - name: hi\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n      allowed_commands: [\"echo\"]\n";

/// The document name of whichever registry entry under `runs_dir` has
/// the earliest `started_at` -- Select's own row order (registry
/// `started_at`, launch order), so this is also the cursor's own
/// row, the first one, with no navigation.
fn earliest_registered_document(runs_dir: &Path) -> String {
    let mut best: Option<(String, String)> = None;
    for entry in std::fs::read_dir(runs_dir)
        .expect("read runs_dir")
        .flatten()
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let (Some(started_at), Some(orchestration)) = (
            value.get("started_at").and_then(|v| v.as_str()),
            value.get("orchestration").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let doc = Path::new(orchestration)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| orchestration.to_string());
        if best.as_ref().is_none_or(|(ts, _)| started_at < ts.as_str()) {
            best = Some((started_at.to_string(), doc));
        }
    }
    best.map(|(_, doc)| doc)
        .unwrap_or_else(|| panic!("no registry entries under {runs_dir:?}"))
}

/// The screen's own line that carries `needle` -- `screen` is already
/// one `\n`-joined [`support::screen_grid`] replay, so a line search
/// is enough to tell "the header row that says TAPE" apart from any
/// other row that merely also mentions a document's own name.
fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{screen}"))
}

/// Issue #698 (design v1 part 5): with two live runs registered, bare
/// `osi` on a TTY opens the Select screen listing both; `enter` on
/// the cursor's own row (the first run, launch order) attaches to it
/// and the chrome's own header names exactly that run, not either
/// one; `esc` then returns to Select with no stray text left over
/// from the attach (the shared `Terminal` built fresh for each leg of
/// this round trip is cleared, not merely redrawn on top of
/// whatever's already on screen); `q` from Select quits at once, with
/// no confirm.
#[test]
fn bare_osi_lists_two_live_runs_and_the_esc_round_trip_returns_to_select() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = SELECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc_a = write_doc(work.path(), "a.yml", SELECT_SLOW_DOC);
    let doc_b = write_doc(work.path(), "b.yml", SELECT_SLOW_DOC);
    let config_a = write_doc(work.path(), "config-a.json", SCRIPTED_CONFIG);
    let config_b = write_doc(work.path(), "config-b.json", SCRIPTED_CONFIG);
    let out_a = tempfile::tempdir().unwrap();
    let out_b = tempfile::tempdir().unwrap();
    let child_a = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc_a,
        &config_a,
        out_a.path(),
    ));
    let child_b = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc_b,
        &config_b,
        out_b.path(),
    ));

    wait_for_step_start(
        &out_a.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    wait_for_step_start(
        &out_b.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    let runs_dir = home.path().join("runs");
    wait_for_registry_entries(&runs_dir, 2, Duration::from_secs(10));
    assert_eq!(
        std::fs::read_dir(&runs_dir)
            .map(|rd| rd.flatten().count())
            .unwrap_or(0),
        2,
        "expected two registry entries while both runs are live, in {runs_dir:?}"
    );
    let cursor_doc = earliest_registered_document(&runs_dir);
    let other_doc = if cursor_doc == "a.yml" {
        "b.yml"
    } else {
        "a.yml"
    };

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        screen.contains("a.yml") && screen.contains("b.yml"),
        "expected both runs listed on Select:\n{screen}"
    );

    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text(&captured_buf, "TAPE", Duration::from_secs(20));
    let tape_line = line_with(&screen, "TAPE");
    assert!(
        tape_line.contains(&cursor_doc) && !tape_line.contains(other_doc),
        "expected enter to attach to the cursor's own run ({cursor_doc}), not {other_doc}:\n{tape_line}"
    );

    send_bytes(&mut master, b"\x1b");
    // Waits for the Select screen to actually redraw (`SELECT`, not
    // merely `TAPE` gone) before checking for stray leftover text --
    // `terminal.clear()` alone would already satisfy "TAPE gone" on
    // the very next, still-blank frame, well before Select's own
    // content is back on screen.
    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        !screen.contains("TAPE"),
        "expected esc to leave no stray Tape text behind on Select:\n{screen}"
    );
    assert!(
        screen.contains("a.yml") && screen.contains("b.yml"),
        "expected esc to return to a freshly drawn Select, both runs still listed:\n{screen}"
    );

    send_bytes(&mut master, b"\r");
    wait_for_screen_text(&captured_buf, "TAPE", Duration::from_secs(20));

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(30));
    assert!(
        status.success(),
        "bare osi should exit 0 after q from an attached run, got {status:?}"
    );

    drop(child_a);
    drop(child_b);
    // `cof`'s own argv carries `--out-dir`/`--events`/`--live-state`
    // under `out_a`/`out_b`, not `work` (`launch_osi_log`'s own
    // `--out-dir`, a separate temp directory) -- the needle this
    // module's own `discover_runs` process scan would actually match
    // a straggler against, so checking `work.path()` alone would
    // miss one and let it bleed into the next locked test.
    assert_no_leftover_process(out_a.path());
    assert_no_leftover_process(out_b.path());
}

/// Every distinct SGR background colour parameter list (`\x1b[...m`
/// where the parameters contain `48;`) in *raw* -- used to check
/// that a theme switch actually painted something different, since
/// this crate's own `screen_grid` strips every escape sequence and
/// keeps only the text.
fn background_codes(raw: &str) -> std::collections::HashSet<String> {
    let mut codes = std::collections::HashSet::new();
    let mut rest = raw;
    while let Some(start) = rest.find("\u{1b}[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find('m') else { break };
        let seq = &rest[..end];
        if seq.contains("48;") {
            codes.insert(seq.to_string());
        }
        rest = &rest[end + 1..];
    }
    codes
}

/// A document with one scripted prompt -- tokens for the Store
/// screen's own leaf tile -- followed by a long-running tool step,
/// so the run stays live through this test's own navigation;
/// document-level `adapter`/`model`, paired with [`scripted_config`]'s
/// own replies file, the same shape `e2e_tui_pty.rs`'s own Store test
/// uses.
const SELECT_THEME_DOC: &str = "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"60\"]\n      allowed_commands: [\"sleep\"]\n";

// 400 tokens, not a handful: the area diagram's own token unit (50
// t/cell with no history) would round a tiny leaf's rect down to
// zero width, which `fill_rect` then skips outright -- no fill for
// this test's own background checks to find.
const SELECT_THEME_REPLIES: &str =
    "prime.ask:\n  - text: \"hi there\"\n    tokens_sent: 200\n    tokens_received: 200\n";

/// Issue #714: `T` cycles the theme on Select, survives the attach
/// into a run, cycles again on that run's own Store, through the All
/// screen opened from Select's own `All` row, and back to Select --
/// one shared value across every leg of the round trip, never reset
/// on any hop. Each press is checked by the theme banner (`theme:
/// <name>`) naming the *next* theme in the cycle from wherever the
/// previous press left it, which only happens if the value actually
/// carried across the hop rather than resetting to `terminal` and
/// re-advancing from there -- every wait's own returned screen is
/// asserted on, not merely discarded, so a banner that never appears
/// actually fails the test. Before any switch, the Store's own
/// finished-prompt leaf tile is checked for a bright-black fill (SGR
/// `100`), the only ANSI colour `terminal` paints it with (issue
/// #714); after the first switch, the header band is checked for a
/// truecolour background (`48;2;`), which `terminal` never emits.
#[test]
fn t_cycles_the_theme_across_select_a_run_and_its_store() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = SELECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // Two live runs, not one: with exactly one, bare `osi` attaches
    // at once (`bare_osi_attaches_at_once_with_exactly_one_live_run`,
    // below) and the interactive Select screen never draws at all,
    // so `enter` on it would never actually set `opened_from_select`
    // -- this test's own point.
    let doc_a = write_doc(work.path(), "a.yml", SELECT_THEME_DOC);
    let doc_b = write_doc(work.path(), "b.yml", SELECT_THEME_DOC);
    let replies_a = write_doc(work.path(), "replies-a.yaml", SELECT_THEME_REPLIES);
    let replies_b = write_doc(work.path(), "replies-b.yaml", SELECT_THEME_REPLIES);
    let config_a = write_doc(work.path(), "config-a.json", &scripted_config(&replies_a));
    let config_b = write_doc(work.path(), "config-b.json", &scripted_config(&replies_b));
    let out_a = tempfile::tempdir().unwrap();
    let out_b = tempfile::tempdir().unwrap();
    let child_a = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc_a,
        &config_a,
        out_a.path(),
    ));
    let child_b = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc_b,
        &config_b,
        out_b.path(),
    ));

    wait_for_step_start(
        &out_a.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    wait_for_step_start(
        &out_b.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    let runs_dir = home.path().join("runs");
    wait_for_registry_entries(&runs_dir, 2, Duration::from_secs(10));
    let cursor_doc = earliest_registered_document(&runs_dir);
    // The finished prompt's own tokens land in the run's live state,
    // polled on its own tick -- a plain event wait only proves the
    // *next* step started, not that the live-state poll already
    // caught up with the one before it (`pressing_3_during_a_live_
    // run_opens_the_store_screen`'s own same `SIGNAL_DELAY`).
    std::thread::sleep(SIGNAL_DELAY);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        screen.contains("SELECT") && screen.contains("a.yml") && screen.contains("b.yml"),
        "expected both runs listed on Select:\n{screen}"
    );

    // Attach to the cursor's own run (row 1, the earliest registered)
    // while still `terminal`.
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text(&captured_buf, "TAPE", Duration::from_secs(20));
    assert!(
        screen.contains("TAPE") && line_with(&screen, "TAPE").contains(&cursor_doc),
        "expected enter to attach to the cursor's own run ({cursor_doc}):\n{screen}"
    );

    // Open the Store, still `terminal`: the finished prompt's own
    // leaf tile must carry a bright-black fill, the only way its
    // area reads against the pane in this theme.
    send_bytes(&mut master, b"3");
    let screen = wait_for_screen_text(&captured_buf, "STORE", Duration::from_secs(10));
    assert!(
        screen.contains("STORE") && screen.contains("ask · 400 t"),
        "expected the Store screen with the finished prompt's own tile:\n{screen}"
    );
    // This pty's own `TERM=xterm-256color` (`spawn_osi_on_pty`'s own
    // env, no `COLORTERM`) resolves to [`theme::ColorMode::Indexed256`],
    // so even `Color::DarkGray` leaves crossterm as an indexed SGR
    // background (`48;5;8`, ANSI colour 8, "bright black"), never the
    // plain `100` a truecolour-capable terminal would use for the same
    // named colour.
    let before_switch = background_codes(&String::from_utf8_lossy(&captured_buf.lock().unwrap()));
    assert!(
        before_switch.iter().any(|c| c.contains("48;5;8")),
        "expected the Store's leaf tile to carry a bright-black (indexed \
         ANSI colour 8) background while the theme is still terminal, got: \
         {before_switch:?}"
    );

    // terminal -> dracula, on the Store: a theme other than `terminal`
    // also downgrades to indexed-256 in this environment, so the
    // switch is checked by a *new* background colour appearing, not a
    // truecolour one specifically.
    send_bytes(&mut master, b"T");
    let screen = wait_for_screen_text(&captured_buf, "theme: dracula", Duration::from_secs(10));
    assert!(
        screen.contains("theme: dracula"),
        "expected the theme banner after T on the Store:\n{screen}"
    );
    let after_switch = background_codes(&String::from_utf8_lossy(&captured_buf.lock().unwrap()));
    assert!(
        after_switch.difference(&before_switch).next().is_some(),
        "expected a background colour on screen that wasn't there before \
         the switch away from terminal, got the same set: {after_switch:?}"
    );

    // esc back to Select: dracula must still be the theme.
    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        screen.contains("SELECT"),
        "expected esc from the Store back to Select:\n{screen}"
    );

    // dracula -> nord, back on Select -- only reachable if esc kept
    // dracula rather than resetting to terminal (which would land
    // back on dracula here instead).
    send_bytes(&mut master, b"T");
    let screen = wait_for_screen_text(&captured_buf, "theme: nord", Duration::from_secs(10));
    assert!(
        screen.contains("theme: nord"),
        "expected the theme banner after T on Select:\n{screen}"
    );

    // Home to the All row (row 0), enter opens it: nord must still
    // be the theme there.
    send_bytes(&mut master, b"g");
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text(&captured_buf, "ALL", Duration::from_secs(20));
    assert!(
        screen.contains("ALL"),
        "expected enter on the All row to open All:\n{screen}"
    );

    // nord -> solarized-dark, on All -- the banner must show there
    // too, same as every other screen.
    send_bytes(&mut master, b"T");
    let screen = wait_for_screen_text(
        &captured_buf,
        "theme: solarized-dark",
        Duration::from_secs(10),
    );
    assert!(
        screen.contains("theme: solarized-dark"),
        "expected the theme banner after T on All:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        screen.contains("SELECT"),
        "expected esc from All back to Select:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(30));
    assert!(
        status.success(),
        "q from Select should exit 0, got {status:?}"
    );

    drop(child_a);
    drop(child_b);
    assert_no_leftover_process(out_a.path());
    assert_no_leftover_process(out_b.path());
}

/// With exactly one live run registered, bare `osi` on a TTY
/// attaches to it straight away -- the Select screen never draws at
/// all.
#[test]
fn bare_osi_attaches_at_once_with_exactly_one_live_run() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = SELECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "a.yml", SELECT_SLOW_DOC);
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);
    let out = tempfile::tempdir().unwrap();
    let launcher = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc,
        &config,
        out.path(),
    ));

    wait_for_step_start(
        &out.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    let runs_dir = home.path().join("runs");
    wait_for_registry_entries(&runs_dir, 1, Duration::from_secs(10));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    let screen = wait_for_screen_text(&captured_buf, "TAPE", Duration::from_secs(20));
    assert!(
        !screen.contains("SELECT"),
        "expected a single live run to attach straight away, never showing Select:\n{screen}"
    );
    assert!(
        line_with(&screen, "TAPE").contains("a.yml"),
        "expected the lone run's own document on the Tape header:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(30));
    assert!(
        status.success(),
        "a voluntary q on a still-live attached run should exit 0, got {status:?}"
    );

    drop(launcher);
    // See the two-run test's own comment: the needle has to be
    // `out`, not `work`.
    assert_no_leftover_process(out.path());
}

/// With no live run registered, bare `osi` on a TTY shows the
/// empty Select's own Recent Runs table,
/// built from this run's own history; `enter` on its one row opens
/// that run read-only.
#[test]
fn bare_osi_shows_the_empty_select_with_a_recorded_run() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = SELECT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "quick.yml", QUICK_DOC);
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);
    let out = tempfile::tempdir().unwrap();
    let child = osi_command(&home)
        .current_dir(work.path())
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(out.path())
        .arg("--log")
        .spawn()
        .expect("spawn osi --log");
    let (status, _stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));
    assert!(
        status.success(),
        "the recorded run should finish ok, got {status:?}"
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    let screen = wait_for_screen_text(&captured_buf, "Recent Runs", Duration::from_secs(20));
    assert!(
        screen.contains("quick.yml"),
        "expected the finished run on the empty Select's own Recent Runs table:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(30));
    assert!(
        status.success(),
        "q on the empty Select should quit at once with exit 0, got {status:?}"
    );

    assert_no_leftover_process(work.path());
}
