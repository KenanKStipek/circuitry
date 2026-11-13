//! A PTY test for issue #752: `osi <doc> --label TEXT` shows the
//! label from the header's first frame, in Select's own NAME column
//! once more than one run is discovered, and in the exit summary
//! once the run has finished -- against a fake engine standing in
//! for `cof`/`electricity` (named `electricity` itself, so osi's own
//! engine detection and the registry's own liveness check both see
//! it as one), since neither real engine writes `label` into
//! `run_start` yet.

mod support;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use support::{
    ELECTRICITY_CONFIG, TestHome, assert_no_leftover_process, drain_continuously, open_pty,
    send_bytes, spawn_osi_on_pty_with_env, wait_for_screen_text, wait_with_timeout, write_doc,
};

/// A fake engine standing in for `electricity`, run through Python
/// (`#!/usr/bin/env python3`, the same shape `e2e_tui_pty_signal_
/// long.rs`'s own fake `cof` uses) rather than a shell script, so
/// `ps`'s own reported command line names an interpreter the
/// registry's own liveness check already recognises
/// (`registry::engine_invocation`'s python-prefix branch) with this
/// script's own path as the next token -- a `#!/bin/sh` shebang
/// would instead show `/bin/sh` first, which that check does not
/// know. Writes `OSI_DOUBLE_EVENTS`/`OSI_DOUBLE_RUNNING_STATE` to
/// its own run directory at once (so a labelled `run_start` is on
/// disk almost immediately, well before any reasonable screen-text
/// timeout), then waits -- bounded the way issue #661 requires of
/// every fake, exiting the moment it is reparented (this test
/// process going away) or after 60s either way -- until `OSI_
/// DOUBLE_FINISH_FILE` appears, at which point it writes `OSI_
/// DOUBLE_FINAL_EVENTS`/`_STATE` (a real completed run) and exits. A
/// run this test never finishes this way simply hits the 60s bound
/// once the test itself is done with it.
const ENGINE_DOUBLE_SCRIPT: &str = r#"#!/usr/bin/env python3
import os, sys, time

args = sys.argv[1:]
if args[:1] == ["--help"]:
    print("usage: --events PATH --live-state PATH")
    sys.exit(0)

out = None
label = ""
for i, arg in enumerate(args):
    if arg == "--out" and i + 1 < len(args):
        out = args[i + 1]
    if arg == "--label" and i + 1 < len(args):
        label = args[i + 1]
if out is None:
    sys.exit(2)

run_dir = os.path.dirname(out)


def fill(text):
    # `__PID__` (literal, never a real engine's own pid shape) stands
    # in for this very process's own pid: the registry's own
    # liveness check (`ps -p <run_start's own pid>`) must see the
    # real subprocess, not a value this test's own setup code could
    # have guessed in advance. `__LABEL__` stands in for whatever
    # `--label` this process was actually started with (issue #752):
    # read from this process's own argv, never baked into the fixed
    # text the test handed over as an environment variable, so
    # deleting osi's own `--label` pass-through fails this test.
    return text.replace("__PID__", str(os.getpid())).replace("__LABEL__", label)


with open(os.path.join(run_dir, "events.jsonl"), "w") as f:
    f.write(fill(os.environ["OSI_DOUBLE_EVENTS"]))
with open(os.path.join(run_dir, "state.live.json"), "w") as f:
    f.write(os.environ["OSI_DOUBLE_RUNNING_STATE"])

finish_file = os.environ["OSI_DOUBLE_FINISH_FILE"]
parent = os.getppid()
end = time.time() + 60
while time.time() < end and os.getppid() == parent:
    if os.path.exists(finish_file):
        break
    time.sleep(0.1)
else:
    sys.exit(0)

with open(os.path.join(run_dir, "events.jsonl"), "w") as f:
    f.write(fill(os.environ["OSI_DOUBLE_FINAL_EVENTS"]))
for path in (os.path.join(run_dir, "state.live.json"), out):
    with open(path, "w") as f:
        f.write(os.environ["OSI_DOUBLE_FINAL_STATE"])
sys.exit(int(os.environ.get("OSI_DOUBLE_EXIT", "0")))
"#;

/// Writes [`ENGINE_DOUBLE_SCRIPT`] to `bin_dir/electricity`,
/// executable -- a short-lived child process writes it, so this test
/// process never holds it open for writing (the ETXTBSY concern
/// `e2e_tui_pty.rs`'s own `write_engine_double` documents).
fn write_engine_double(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("electricity");
    let mut writer = Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("start the engine double's writer");
    std::io::Write::write_all(
        &mut writer.stdin.take().unwrap(),
        ENGINE_DOUBLE_SCRIPT.as_bytes(),
    )
    .expect("write engine double");
    assert!(
        writer.wait().unwrap().success(),
        "writing the engine double failed"
    );
    path
}

fn path_with(bin_dir: &Path) -> OsString {
    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).expect("join PATH")
}

/// A `run_start` event line carrying `label`, plus one finished leaf
/// -- the "still running" shape this test's own engine double writes
/// at once. `label` here is always `__LABEL__` (the engine double's
/// own placeholder, filled in from its own argv, never from this
/// text directly) -- see [`ENGINE_DOUBLE_SCRIPT`]'s own `fill`.
fn running_events(run_id: &str) -> String {
    format!(
        concat!(
            "{{\"v\":1,\"seq\":0,\"ts\":\"2026-01-01T00:00:00Z\",\"ev\":\"run_start\",",
            "\"run_id\":\"{run_id}\",\"orchestration\":\"do-thing.yml\",\"pid\":__PID__,",
            "\"engine\":\"electricity 0.1.0\",\"label\":\"__LABEL__\"}}\n",
        ),
        run_id = run_id,
    )
}

fn running_state() -> &'static str {
    concat!(
        "{\"runtime\":{\"last_run\":{\"completed_at\":null}},",
        "\"prime\":{\"value\":null,\"meta\":{\"created_at\":\"2026-01-01T00:00:00Z\",",
        "\"completed_at\":null}}}"
    )
}

fn final_events(run_id: &str) -> String {
    format!(
        concat!(
            "{{\"v\":1,\"seq\":0,\"ts\":\"2026-01-01T00:00:00Z\",\"ev\":\"run_start\",",
            "\"run_id\":\"{run_id}\",\"orchestration\":\"do-thing.yml\",\"pid\":__PID__,",
            "\"engine\":\"electricity 0.1.0\",\"label\":\"__LABEL__\"}}\n",
            "{{\"v\":1,\"seq\":1,\"ts\":\"2026-01-01T00:00:01Z\",\"ev\":\"run_end\",\"ok\":true}}\n",
        ),
        run_id = run_id,
    )
}

fn final_state() -> &'static str {
    concat!(
        "{\"runtime\":{\"last_run\":{\"completed_at\":\"2026-01-01T00:00:01Z\",",
        "\"totals\":{\"wall_time_s\":1.0,\"effects_run\":1,\"tokens_sent\":0,",
        "\"tokens_received\":0,\"cost_usd\":0.0}}},",
        "\"prime\":{\"value\":true,\"meta\":{\"created_at\":\"2026-01-01T00:00:00Z\",",
        "\"completed_at\":\"2026-01-01T00:00:01Z\",\"error\":null}}}"
    )
}

#[test]
fn a_run_started_with_label_shows_it_in_the_header_select_and_exit_summary() {
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_engine_double(bin_dir.path());
    let path_env = path_with(bin_dir.path());

    let doc = write_doc(
        work.path(),
        "do-thing.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let out_dir_a = tempfile::tempdir().unwrap();
    let out_dir_b = tempfile::tempdir().unwrap();
    let finish_file_a = work.path().join("finish-a");

    let (master_a, slave_a) = open_pty();
    let mut master_a = master_a;
    let captured_a = drain_continuously(master_a.try_clone().expect("dup pty master a"));
    let child_a = spawn_osi_on_pty_with_env(
        &home,
        &slave_a,
        &[
            doc.as_os_str(),
            config.as_os_str(),
            OsStr::new("--engine"),
            OsStr::new("electricity"),
            OsStr::new("--out-dir"),
            out_dir_a.path().as_os_str(),
            OsStr::new("--label"),
            OsStr::new("ticket 42"),
        ],
        &[
            ("PATH", path_env.as_os_str()),
            (
                "OSI_DOUBLE_EVENTS",
                OsStr::new(&running_events("run-a")),
            ),
            ("OSI_DOUBLE_RUNNING_STATE", OsStr::new(running_state())),
            ("OSI_DOUBLE_FINISH_FILE", finish_file_a.as_os_str()),
            (
                "OSI_DOUBLE_FINAL_EVENTS",
                OsStr::new(&final_events("run-a")),
            ),
            ("OSI_DOUBLE_FINAL_STATE", OsStr::new(final_state())),
        ],
    );

    // The header shows the label from very nearly its first frame
    // (issue #752's own "shows it from the first frame"): the engine
    // double writes its labelled `run_start` before it does anything
    // else at all.
    wait_for_screen_text(&captured_a, "ticket 42", Duration::from_secs(20));

    let (master_b, slave_b) = open_pty();
    let mut master_b = master_b;
    let captured_b = drain_continuously(master_b.try_clone().expect("dup pty master b"));
    let child_b = spawn_osi_on_pty_with_env(
        &home,
        &slave_b,
        &[
            doc.as_os_str(),
            config.as_os_str(),
            OsStr::new("--engine"),
            OsStr::new("electricity"),
            OsStr::new("--out-dir"),
            out_dir_b.path().as_os_str(),
            OsStr::new("--label"),
            OsStr::new("ticket seven"),
        ],
        &[
            ("PATH", path_env.as_os_str()),
            (
                "OSI_DOUBLE_EVENTS",
                OsStr::new(&running_events("run-b")),
            ),
            ("OSI_DOUBLE_RUNNING_STATE", OsStr::new(running_state())),
            (
                "OSI_DOUBLE_FINISH_FILE",
                work.path().join("finish-b-never").as_os_str(),
            ),
            (
                "OSI_DOUBLE_FINAL_EVENTS",
                OsStr::new(&final_events("run-b")),
            ),
            ("OSI_DOUBLE_FINAL_STATE", OsStr::new(final_state())),
        ],
    );
    wait_for_screen_text(&captured_b, "ticket seven", Duration::from_secs(20));

    // Both runs are still alive now -- bare `osi`, with no document
    // of its own, discovers both through the shared registry (same
    // `TestHome`, so the same `OSI_RUNS_DIR`) and shows the Select
    // screen's own running list, never attaching to just one.
    let (master_c, slave_c) = open_pty();
    let mut master_c = master_c;
    let captured_c = drain_continuously(master_c.try_clone().expect("dup pty master c"));
    let child_c = spawn_osi_on_pty_with_env(&home, &slave_c, &[], &[]);
    let select_screen = wait_for_screen_text(&captured_c, "ticket seven", Duration::from_secs(20));
    assert!(
        select_screen.contains("ticket 42"),
        "expected both labels on Select's own running list:\n{select_screen}"
    );
    assert!(
        select_screen.contains("Running Orchestrations"),
        "expected the Select screen itself, not a single-run attach:\n{select_screen}"
    );

    send_bytes(&mut master_c, b"q");
    let status_c = wait_with_timeout(child_c, Duration::from_secs(30));
    assert!(
        status_c.success(),
        "bare osi should exit 0, got {status_c:?}"
    );

    // Now run A finishes for real: the exit summary (DESIGN.md
    // §2.4's `■ run <name> ...` row) shows the same label, printed
    // to the plain scrollback once the TUI leaves the alt screen.
    std::fs::write(&finish_file_a, b"go").unwrap();
    wait_for_screen_text(&captured_a, "run ticket 42", Duration::from_secs(20));
    send_bytes(&mut master_a, b"q");
    let status_a = wait_with_timeout(child_a, Duration::from_secs(30));
    assert!(status_a.success(), "osi should exit 0, got {status_a:?}");
    let final_text_a = String::from_utf8_lossy(&captured_a.lock().unwrap()).into_owned();
    assert!(
        final_text_a.contains("run ticket 42") && final_text_a.contains("ok"),
        "expected the exit summary to name the run by its label:\n{final_text_a}"
    );

    // B's own engine is still running (it never got a finish file),
    // so `q` opens the cancel-confirm dialog (DESIGN.md: a launch's
    // own `q` while running never quits at once, unlike `osi watch`
    // or an already-ended run) -- `y` confirms it.
    send_bytes(&mut master_b, b"q");
    std::thread::sleep(Duration::from_millis(300));
    send_bytes(&mut master_b, b"y");
    let _status_b = wait_with_timeout(child_b, Duration::from_secs(30));

    assert_no_leftover_process(work.path());
    assert_no_leftover_process(out_dir_a.path());
    assert_no_leftover_process(out_dir_b.path());
}
