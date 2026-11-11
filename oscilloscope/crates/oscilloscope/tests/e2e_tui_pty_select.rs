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
    SCRIPTED_CONFIG, SELECT_SLOW_DOC, TestHome, assert_no_leftover_process, drain_continuously,
    e2e_enabled, launch_osi_log, open_pty, osi_command, send_bytes, spawn_osi_on_pty,
    wait_for_registry_entries, wait_for_screen_text, wait_for_step_start, wait_with_timeout,
    wait_with_timeout_capturing_stdout, write_doc,
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
    let _guard = SELECT_LOCK.lock().unwrap();
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

/// Issue #714: `T` cycles the theme on Select, survives the
/// attach into a run, cycles again on that run's own Store, and
/// survives `esc` back to Select -- one shared value across every
/// leg of the round trip, never reset on either hop. Each press is
/// checked by the theme banner (`theme: <name>`) naming the *next*
/// theme in the cycle from wherever the previous press left it,
/// which only happens if the value actually carried across the hop
/// rather than resetting to `terminal` and re-advancing from there.
#[test]
fn t_cycles_the_theme_across_select_a_run_and_its_store() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = SELECT_LOCK.lock().unwrap();
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

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));

    // terminal -> dracula, on Select.
    send_bytes(&mut master, b"T");
    wait_for_screen_text(&captured_buf, "theme: dracula", Duration::from_secs(10));

    // Attach to a run: dracula must still be the theme the Tape
    // draws in, not reset back to terminal.
    send_bytes(&mut master, b"\r");
    wait_for_screen_text(&captured_buf, "TAPE", Duration::from_secs(20));

    // dracula -> nord, on the Tape -- only reachable if the attach
    // kept dracula rather than resetting to terminal (which would
    // land back on dracula here instead).
    send_bytes(&mut master, b"T");
    wait_for_screen_text(&captured_buf, "theme: nord", Duration::from_secs(10));

    // Open the Store: still nord.
    send_bytes(&mut master, b"3");
    wait_for_screen_text(&captured_buf, "STORE", Duration::from_secs(10));

    // nord -> solarized-dark, on the Store.
    send_bytes(&mut master, b"T");
    wait_for_screen_text(
        &captured_buf,
        "theme: solarized-dark",
        Duration::from_secs(10),
    );

    // esc back to Select: solarized-dark must still be the theme.
    send_bytes(&mut master, b"\x1b");
    wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));

    // solarized-dark -> solarized-light, back on Select -- only
    // reachable if esc kept solarized-dark rather than resetting.
    send_bytes(&mut master, b"T");
    wait_for_screen_text(
        &captured_buf,
        "theme: solarized-light",
        Duration::from_secs(10),
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
    let _guard = SELECT_LOCK.lock().unwrap();
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
    let _guard = SELECT_LOCK.lock().unwrap();
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
