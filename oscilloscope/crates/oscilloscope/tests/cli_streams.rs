//! Which stream `osp`'s own CLI-parsing errors land on (#422 review
//! note): a usage error (exit 2) must print to stderr, same as every
//! other error osp reports; `--help`/`--version` (exit 0) still print
//! to stdout, like any other successful output. Needs no `cof`.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_osp"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn osp")
}

#[test]
fn a_usage_error_prints_to_stderr_not_stdout() {
    let out = run(&["--bogus-flag", "do-thing.yml"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.stderr.is_empty(),
        "the usage error should be on stderr"
    );
}

#[test]
fn no_args_prints_the_usage_error_to_stderr() {
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[test]
fn help_prints_to_stdout_not_stderr() {
    let out = run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!out.stdout.is_empty());
    assert!(
        out.stderr.is_empty(),
        "stderr should be empty: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn version_prints_to_stdout_not_stderr() {
    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn an_existing_world_accessible_out_dir_is_warned_about_not_chmodded() {
    // K7: osp must never force a pre-existing --out-dir to 0700 (it
    // didn't create it, and chmod'ing a directory outside its own run
    // directory is an unrequested, possibly even EPERM-failing,
    // change) -- it only warns on stderr. `--engine electricity` with
    // a config but with the `electricity` binary itself not on PATH
    // (true in this environment, and never assumed otherwise) reaches
    // the launch-failure path (exit 1) only *after* `create_run_dir`
    // has already run (P2-1 moved it to immediately before the
    // spawn) -- with no `cof` needed at all, so this never touches the
    // owner's own real `cof`/HOME either (F11).
    let work = tempfile::tempdir().unwrap();
    let doc = work.path().join("do.yml");
    std::fs::write(&doc, "effects: []\n").unwrap();
    let config = work.path().join("config.json");
    std::fs::write(&config, "{}").unwrap();
    let out_dir = work.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    std::fs::set_permissions(&out_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_osp"))
        .arg(&doc)
        .arg(&config)
        .arg("--engine")
        .arg("electricity")
        .arg("--out-dir")
        .arg(&out_dir)
        .stdin(Stdio::null())
        .output()
        .expect("spawn osp");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("group- or world-accessible"),
        "stderr:\n{stderr}"
    );
    let mode = std::fs::metadata(&out_dir).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o777,
        "a pre-existing out-dir's mode must be left untouched"
    );
}

#[test]
fn watch_with_no_events_stream_warns_once_instead_of_hanging_silently() {
    // N3: a run directory from a bare `cof run --live-state ... --out
    // ...` (no `--events`) gives `osp watch` no pid to probe, and an
    // aborted run never writes a completed state either — the
    // orchestrator's decision here is no staleness timeout (a quiet
    // run can be quiet for a long time), just a once-only stderr
    // notice of what watch is relying on instead (Ctrl-C), so a user
    // attached to a run that goes on to abort isn't left looking at a
    // silent hang with no explanation at all. This run deliberately
    // never ends (no `runtime.last_run.completed_at`, no `events
    // .jsonl`), so the test kills `osp watch` itself once the warning
    // has been seen rather than waiting for it to exit on its own.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("state.live.json"),
        r#"{"runtime":{"last_run":{"completed_at":null}},"prime":{"value":null,"meta":{"completed_at":null}}}"#,
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_osp"))
        .arg("watch")
        .arg(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn osp watch");

    // A plain blocking `read` can't be bounded by this test's own
    // deadline once it's already inside the call, so the read itself
    // runs on its own thread: the main thread polls that thread (which
    // osp watch, left running forever with nothing left to report,
    // can't ever finish on its own) against a hard deadline instead.
    let mut stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut collected = String::new();
        loop {
            match stderr.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    collected.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if collected.contains("--events") && collected.contains("Ctrl-C") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(collected);
    });

    let collected = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| "<no output within 5s>".to_string());

    let _ = child.kill();
    let _ = child.wait();

    assert!(collected.contains("--events"), "stderr:\n{collected}");
    assert!(collected.contains("Ctrl-C"), "stderr:\n{collected}");
}
