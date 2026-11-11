//! End-to-end tests for the All screen (issue #698, design v1 part
//! 5) against a real `cof`, on a real pseudo-terminal -- gated and
//! isolated the same way `e2e_tui_pty_select.rs` is: `OSI_E2E_COF=1`
//! and `cof` on `PATH`, a temporary `HOME`, stripped credentials, a
//! hard per-test timeout, and this file's own lock, for the same
//! `ps`-scan-sensitivity reason that module's own doc comment gives.

mod support;

use std::process::Child;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use support::{
    SCRIPTED_CONFIG, SELECT_SLOW_DOC, TestHome, assert_no_leftover_process, drain_continuously,
    e2e_enabled, launch_osi_log, open_pty, send_bytes, spawn_osi_on_pty, wait_for_registry_entries,
    wait_for_screen_text, wait_for_screen_text_gone, wait_for_step_start, wait_with_timeout,
    write_doc,
};

static ALL_LOCK: Mutex<()> = Mutex::new(());

/// See `e2e_tui_pty_select.rs`'s own `AutoKill` doc comment: the
/// same guard, so a panic partway through this file's own round
/// trip can never leave a background `cof` running for a later
/// test's own `ps` scan to find.
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

/// Issue #698, §4's own navigation path: with two live runs
/// registered (both fit on one page at this terminal's own width,
/// so arrow focus has somewhere real to move to), bare `osi` on a
/// TTY lists both plus `All` on Select; `g` moves the cursor to
/// `All`'s own row (row 0); `enter` opens the grid (the `ALL` chip);
/// `1` switches every tile to Signal; `→` moves the focus from the
/// first tile to the second; `enter` opens whichever run is
/// focused, proving the move really landed there, not just that the
/// key was accepted; `esc` returns to the grid, with the focus still
/// on the second tile (another `enter` there opens the same run
/// again, not the grid's own default focus); a further `esc`
/// returns to Select, with every run still listed; `q` from Select
/// quits at once, with no confirm.
#[test]
fn bare_osi_opens_all_and_the_esc_round_trip_returns_to_select() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = ALL_LOCK.lock().unwrap();
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

    for out in [&out_a, &out_b] {
        wait_for_step_start(
            &out.path().join("events.jsonl"),
            "prime.slow",
            Duration::from_secs(20),
        );
    }
    let runs_dir = home.path().join("runs");
    wait_for_registry_entries(&runs_dir, 2, Duration::from_secs(10));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        screen.contains("All"),
        "expected the All row listed on Select:\n{screen}"
    );

    send_bytes(&mut master, b"g");
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text(&captured_buf, "ALL", Duration::from_secs(20));
    assert!(
        screen.contains("ALL"),
        "expected enter on the All row to open the grid:\n{screen}"
    );

    send_bytes(&mut master, b"1");
    let screen = wait_for_screen_text(&captured_buf, "SIGNAL", Duration::from_secs(20));
    assert!(
        screen.contains("SIGNAL"),
        "expected 1 to switch every tile to Signal:\n{screen}"
    );

    // `enter` at the grid's own default focus (index 0), to learn
    // which of the two documents is at that position --
    // discovery's own launch-order tie-break between
    // two runs started a moment apart is real timing, not something
    // this test can assume either way.
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text_gone(&captured_buf, "osi  All", Duration::from_secs(20));
    assert!(
        screen.contains("SIGNAL") && !screen.contains("osi  All"),
        "expected enter to open the focused run on the screen every tile was already showing \
         (Signal), not back on Tape:\n{screen}"
    );
    let first_doc = if screen.contains("a.yml") {
        "a.yml"
    } else if screen.contains("b.yml") {
        "b.yml"
    } else {
        panic!("expected the opened run's own header to name a.yml or b.yml:\n{screen}");
    };

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "osi  All", Duration::from_secs(20));
    assert!(
        screen.contains("ALL"),
        "expected esc from the opened run to return to the grid:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b[C"); // Right arrow: focus the other tile.
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text_gone(&captured_buf, "osi  All", Duration::from_secs(20));
    let second_doc = if first_doc == "a.yml" {
        "b.yml"
    } else {
        "a.yml"
    };
    assert!(
        screen.contains(second_doc) && !screen.contains(first_doc) && !screen.contains("osi  All"),
        "expected the right arrow to move focus to the other tile, so this enter opens \
         {second_doc}, not {first_doc} again:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "osi  All", Duration::from_secs(20));
    assert!(
        screen.contains("ALL"),
        "expected esc from the opened run to return to the grid:\n{screen}"
    );

    // The focus itself survived that round trip: `enter` here, with
    // no further navigation, opens the same tile `\x1b[C` moved to
    // above (`second_doc`) again -- not the grid's own default
    // focus (index 0), which pressing `\x1b[C` then `enter` would
    // also reach from a freshly reset focus, so this is the one
    // check that actually tells the two apart.
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text_gone(&captured_buf, "osi  All", Duration::from_secs(20));
    assert!(
        screen.contains(second_doc) && !screen.contains(first_doc) && !screen.contains("osi  All"),
        "expected the grid's own focus to still be on {second_doc} after the esc round trip, \
         not reset to the default focus:\n{screen}"
    );
    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "osi  All", Duration::from_secs(20));
    assert!(
        screen.contains("ALL"),
        "expected esc from the opened run to return to the grid:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(20));
    assert!(
        screen.contains("SELECT") && !screen.contains("osi  All"),
        "expected esc from the grid to return to Select, not stay on All:\n{screen}"
    );
    assert!(
        screen.contains("a.yml") && screen.contains("b.yml"),
        "expected every run still listed on Select after the round trip:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(30));
    assert!(
        status.success(),
        "q on Select should quit at once with exit 0, got {status:?}"
    );

    drop(child_a);
    drop(child_b);
    // See `e2e_tui_pty_select.rs`'s own comment: the needle has to
    // be each run's own `out_dir`, not the shared `work` directory.
    assert_no_leftover_process(out_a.path());
    assert_no_leftover_process(out_b.path());
}
