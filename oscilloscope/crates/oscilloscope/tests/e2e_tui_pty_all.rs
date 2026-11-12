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
    e2e_enabled, launch_osi_log, open_pty, resize_pty, send_bytes, spawn_osi_on_pty,
    wait_for_registry_entries, wait_for_screen_text, wait_for_screen_text_gone,
    wait_for_step_start, wait_with_timeout, write_doc,
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

/// The screen's own line that carries `needle` -- `screen` is
/// already one `\n`-joined [`support::screen_grid`] replay, so a
/// line search is enough to tell one header row apart from any
/// other row that merely also mentions a document's own name.
fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{screen}"))
}

/// Issue #698's own `u`: with three live runs, Select → All → `u`
/// opens the Combined Signal; `↓` moves its own cursor off the
/// first run; `enter` opens that run full-screen on Signal, the Per
/// Run table's own second row named in the header, not its first;
/// `esc` from it returns to a fresh Combined leg; `esc` from
/// Combined returns to the tiles, with the grid's own page
/// untouched by the round trip (checked by paging to the grid's own
/// second page beforehand and confirming the status line still
/// names it afterwards, not the first page); `esc` from the grid
/// returns to Select; `q` quits at once.
#[test]
fn bare_osi_opens_the_combined_signal_and_the_esc_round_trip_returns_to_select() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = ALL_LOCK.lock().unwrap();
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();

    let doc_names = ["a.yml", "b.yml", "c.yml"];
    let docs: Vec<_> = doc_names
        .iter()
        .map(|name| write_doc(work.path(), name, SELECT_SLOW_DOC))
        .collect();
    let configs: Vec<_> = ["config-a.json", "config-b.json", "config-c.json"]
        .iter()
        .map(|name| write_doc(work.path(), name, SCRIPTED_CONFIG))
        .collect();
    let outs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let children: Vec<AutoKill> = (0..3)
        .map(|i| {
            AutoKill::new(launch_osi_log(
                &home,
                work.path(),
                &docs[i],
                &configs[i],
                outs[i].path(),
            ))
        })
        .collect();

    for out in &outs {
        wait_for_step_start(
            &out.path().join("events.jsonl"),
            "prime.slow",
            Duration::from_secs(10),
        );
    }
    let runs_dir = home.path().join("runs");
    wait_for_registry_entries(&runs_dir, 3, Duration::from_secs(10));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(10));
    assert!(
        screen.contains("All"),
        "expected the All row listed on Select:\n{screen}"
    );

    send_bytes(&mut master, b"g");
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text(&captured_buf, "ALL", Duration::from_secs(10));
    assert!(
        screen.contains("ALL"),
        "expected enter on the All row to open the grid:\n{screen}"
    );
    // Three runs at this terminal's own size page one tile per
    // page -- the status line's own `… N more` names how many runs
    // are *not* on the current page, which differs page to page (2
    // on the first, 1 on the second, none on the third), so it
    // names the page itself without needing a specific run's own
    // identity to survive a round trip (`discover_running`'s own
    // order is free to re-settle a near-simultaneous launch's own
    // tie between two scans, even without anything in between
    // touching the grid at all).
    assert!(
        screen.contains("… 2 more"),
        "expected the first page to name 2 more runs:\n{screen}"
    );
    send_bytes(&mut master, b"\x1b[6~"); // PageDown.
    wait_for_screen_text(&captured_buf, "… 1 more", Duration::from_secs(10));

    send_bytes(&mut master, b"u");
    let screen = wait_for_screen_text(&captured_buf, "COMBINED SIGNAL", Duration::from_secs(10));
    assert!(
        screen.contains("Per Run") && screen.contains("Readings"),
        "expected u to open the Combined Signal:\n{screen}"
    );
    // The Per Run table's own second row, top to bottom, read
    // straight off this screen rather than assumed from launch
    // order -- three runs launched this close together don't
    // reliably settle a registry-time tie-break the same way
    // every time. The chart's own legend, above the "Per Run"
    // title, names every run too, so only lines after that title
    // are candidates.
    let per_run_title_at = screen
        .lines()
        .position(|l| l.contains("Per Run"))
        .unwrap_or_else(|| panic!("expected a \"Per Run\" title:\n{screen}"));
    // Only the part of the row before the Per Run/Readings border
    // (`││`) is this table's own text -- the Readings panel beside
    // it names runs too ("Largest a.yml"), on the very same
    // terminal row as one of this table's own rows.
    let per_run_cell = |l: &str| l.split("\u{2502}\u{2502}").next().unwrap_or(l).to_string();
    let second_row = screen
        .lines()
        .skip(per_run_title_at + 1)
        .map(per_run_cell)
        .filter(|l| doc_names.iter().any(|d| l.contains(d)))
        .nth(1)
        .unwrap_or_else(|| panic!("expected a second Per Run row:\n{screen}"));
    let second_combined_doc = *doc_names
        .iter()
        .find(|&&doc| second_row.contains(doc))
        .unwrap_or_else(|| {
            panic!("expected the second Per Run row to name one of the three runs:\n{second_row}")
        });

    send_bytes(&mut master, b"\x1b[B"); // Down arrow: select the second run.
    send_bytes(&mut master, b"\r");
    // "SIGNAL" alone is a substring of "COMBINED SIGNAL", so this
    // waits for the Combined chip to actually disappear, not merely
    // for a needle the Combined screen itself already satisfies.
    let screen =
        wait_for_screen_text_gone(&captured_buf, "COMBINED SIGNAL", Duration::from_secs(10));
    assert!(
        screen.contains("SIGNAL") && !screen.contains("COMBINED SIGNAL"),
        "expected enter on the selected run to open it full-screen on Signal:\n{screen}"
    );
    // This would still pass if `↓` had been silently ignored and
    // `enter` simply reopened the first row's own run instead, so
    // the document itself is checked against the Per Run table's
    // own second row, read above, not only that some Signal screen
    // opened.
    assert!(
        line_with(&screen, "SIGNAL").contains(second_combined_doc),
        "expected enter after ↓ to open the Per Run table's own second row ({second_combined_doc}), not the first:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "COMBINED SIGNAL", Duration::from_secs(10));
    assert!(
        screen.contains("COMBINED SIGNAL"),
        "expected esc from the opened run to return to the Combined Signal:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen =
        wait_for_screen_text_gone(&captured_buf, "COMBINED SIGNAL", Duration::from_secs(10));
    assert!(
        screen.contains("osi  All") && !screen.contains("COMBINED SIGNAL"),
        "expected esc from the Combined Signal to return to the tiles:\n{screen}"
    );
    // The grid's own page survived the round trip: still the
    // second page (… 1 more), not reset back to the first (… 2
    // more).
    assert!(
        screen.contains("… 1 more") && !screen.contains("… 2 more"),
        "expected the grid's own page to survive the Combined round trip:\n{screen}"
    );

    // `enter` here, with no `PageDown`/`PageUp` touched since the
    // round trip began, opens whichever run the still-second page
    // shows.
    // "TAPE" alone is already on screen (the grid's own "ALL ·
    // TAPE" chip), so this waits for that chip to actually change,
    // not merely for a needle the grid itself already satisfies.
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text_gone(&captured_buf, "ALL · TAPE", Duration::from_secs(10));
    assert!(
        doc_names.iter().any(|doc| screen.contains(doc)),
        "expected enter on the grid to open one of the three runs:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "ALL · TAPE", Duration::from_secs(10));
    assert!(
        screen.contains("osi  All") && screen.contains("ALL · TAPE"),
        "expected esc from the reopened run to return to the tiles:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(10));
    assert!(
        screen.contains("SELECT") && !screen.contains("osi  All"),
        "expected esc from the grid to return to Select, not stay on All:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(20));
    assert!(
        status.success(),
        "q on Select should quit at once with exit 0, got {status:?}"
    );

    drop(children);
    for out in &outs {
        assert_no_leftover_process(out.path());
    }
}

/// Below the Combined Signal's own floor (narrower than 120
/// columns, here, rather than shorter than its own rows), `u`
/// bounces straight back to the tiles with a status note, the same
/// way the single-run Signal screen bounces to the Tape below 80
/// columns -- never the shared too-small notice, which stays for a
/// terminal too small for any screen at all.
#[test]
fn combined_signal_bounces_to_the_tiles_below_its_own_floor() {
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
            Duration::from_secs(10),
        );
    }
    let runs_dir = home.path().join("runs");
    wait_for_registry_entries(&runs_dir, 2, Duration::from_secs(10));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let select_child = AutoKill::new(spawn_osi_on_pty(&home, &slave_path, &[]));

    wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(10));
    send_bytes(&mut master, b"g");
    send_bytes(&mut master, b"\r");
    wait_for_screen_text(&captured_buf, "ALL · TAPE", Duration::from_secs(10));

    // Narrower than the Combined Signal's own floor (120 columns),
    // but still well above the shared 60x24 floor every screen has.
    resize_pty(&slave_path, 100, 40);
    send_bytes(&mut master, b"u");
    let screen = wait_for_screen_text(
        &captured_buf,
        "Combined Signal needs",
        Duration::from_secs(10),
    );
    assert!(
        screen.contains("osi  All")
            && screen.contains("ALL · TAPE")
            && !screen.contains("COMBINED SIGNAL"),
        "expected u to bounce straight back to the tiles, not open the Combined Signal:\n{screen}"
    );
    assert!(
        !screen.contains("osi needs"),
        "expected the Combined Signal's own note, not the shared too-small notice:\n{screen}"
    );

    // Back to a size the Combined Signal does fit: `u` opens it
    // normally once the terminal is wide enough again.
    resize_pty(&slave_path, 120, 40);
    send_bytes(&mut master, b"u");
    let screen = wait_for_screen_text(&captured_buf, "COMBINED SIGNAL", Duration::from_secs(10));
    assert!(
        screen.contains("COMBINED SIGNAL"),
        "expected u to open the Combined Signal once the terminal is wide enough:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    wait_for_screen_text_gone(&captured_buf, "COMBINED SIGNAL", Duration::from_secs(10));
    send_bytes(&mut master, b"\x1b");
    let screen = wait_for_screen_text(&captured_buf, "SELECT", Duration::from_secs(10));
    assert!(
        screen.contains("SELECT") && !screen.contains("osi  All"),
        "expected esc from the grid to return to Select:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(20));
    assert!(
        status.success(),
        "q on Select should quit at once with exit 0, got {status:?}"
    );

    drop(child_a);
    drop(child_b);
    assert_no_leftover_process(out_a.path());
    assert_no_leftover_process(out_b.path());
}

/// Every token right after a standalone occurrence of `document` on
/// `screen`, wherever it appears -- Select's own running list has
/// one occurrence per line, but the All grid draws every tile's own
/// band on the *same* screen row, side by side, so a caller can't
/// just filter to the one line that "contains" it and assume there
/// is only one match there.
fn names_following_document<'a>(screen: &'a str, document: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    for line in screen.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for i in 0..tokens.len() {
            if tokens[i] == document {
                if let Some(next) = tokens.get(i + 1) {
                    found.push(*next);
                }
            }
        }
    }
    found
}

/// Issue #717: two live runs of the *same* document and the *same*
/// config -- nothing else on screen tells them apart -- still read
/// as two distinct rows, because each gets its own short name from
/// its own run id. Checked on both Select's own running list and on
/// the All grid's own tile bands, the same way the design issue's
/// own test asks for.
#[test]
fn two_runs_of_the_same_document_and_config_get_different_names_on_select_and_all() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let _guard = ALL_LOCK.lock().unwrap();
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();

    let doc = write_doc(work.path(), "same.yml", SELECT_SLOW_DOC);
    let config = write_doc(work.path(), "same-config.json", SCRIPTED_CONFIG);
    let out_a = tempfile::tempdir().unwrap();
    let out_b = tempfile::tempdir().unwrap();
    let child_a = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc,
        &config,
        out_a.path(),
    ));
    let child_b = AutoKill::new(launch_osi_log(
        &home,
        work.path(),
        &doc,
        &config,
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
    let names = names_following_document(&screen, "same.yml");
    assert_eq!(
        names.len(),
        2,
        "expected two rows naming same.yml on Select:\n{screen}"
    );
    assert_ne!(
        names[0], names[1],
        "expected the two same.yml rows to carry different names on Select:\n{screen}"
    );
    assert_ne!(
        names[0], "same-config.json",
        "expected a name, not the config, right after the document"
    );

    send_bytes(&mut master, b"g");
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text(&captured_buf, "ALL", Duration::from_secs(20));
    let tile_names = names_following_document(&screen, "same.yml");
    assert_eq!(
        tile_names.len(),
        2,
        "expected two tile bands naming same.yml on All:\n{screen}"
    );
    assert_ne!(
        tile_names[0], tile_names[1],
        "expected the two same.yml tile bands to carry different names on All:\n{screen}"
    );
    assert_eq!(
        names.into_iter().collect::<std::collections::HashSet<_>>(),
        tile_names
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        "expected All's own two names to be the same pair Select showed:\n{screen}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(select_child.into_inner(), Duration::from_secs(20));
    assert!(
        status.success(),
        "q on the grid should quit at once with exit 0, got {status:?}"
    );

    drop(child_a);
    drop(child_b);
    assert_no_leftover_process(out_a.path());
    assert_no_leftover_process(out_b.path());
}
