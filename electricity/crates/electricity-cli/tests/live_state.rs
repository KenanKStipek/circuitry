//! Mid-run `--live-state` integration test (issue #431's acceptance
//! criteria: a watcher tailing `--live-state` while a run is in
//! progress never sees a torn or stale-forever file, and periodic
//! writes actually land roughly [`electricity::live_state::
//! LIVE_STATE_INTERVAL`] apart).
//!
//! `#![cfg(feature = "test-tools")]`, same reasoning as `signals.rs`:
//! this needs the test-only `sleep` provider, so it never builds under
//! the ordinary `cargo test --workspace` gate.
//!
//! Run explicitly:
//! ```sh
//! cargo test -p electricity-cli --features test-tools --test live_state
//! ```

#![cfg(feature = "test-tools")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "electricity-live-state-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TempHome { path }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Six sequential (chain, the default) `sleep` steps of 0.3s each --
/// `~1.8s` total, long enough against `LIVE_STATE_INTERVAL`'s `500ms`
/// to see several periodic writes land roughly that far apart before
/// the run ends.
const SIX_SLEEPS_DOC: &str = "\
effects:
  - name: s1
    type: tool
    provider: sleep
    params: {seconds: 0.3}
  - name: s2
    type: tool
    provider: sleep
    params: {seconds: 0.3}
  - name: s3
    type: tool
    provider: sleep
    params: {seconds: 0.3}
  - name: s4
    type: tool
    provider: sleep
    params: {seconds: 0.3}
  - name: s5
    type: tool
    provider: sleep
    params: {seconds: 0.3}
  - name: s6
    type: tool
    provider: sleep
    params: {seconds: 0.3}
";

/// Kills *child* (its whole process group) on drop, including when a
/// panicking assertion unwinds through it, so a hung/sleeping child is
/// never left running after the test that started it ends
/// (`LANE-CONTRACT.md`'s own rule; same approach as `signals.rs`'s
/// `TestChild`).
struct TestChild {
    child: std::process::Child,
}

impl Drop for TestChild {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn spawn(home: &TempHome, config: &Path, doc: &Path, live: &Path, out: &Path) -> TestChild {
    use std::os::unix::process::CommandExt as _;
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.args([config.to_str().unwrap(), doc.to_str().unwrap()]);
    cmd.args(["--live-state", live.to_str().unwrap()]);
    cmd.args(["--out", out.to_str().unwrap()]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    TestChild {
        child: cmd.spawn().expect("spawn electricity"),
    }
}

/// One mid-run `--live-state` run: polls *live* every ~10ms until the
/// child exits (plus one read right after, to catch a final `close()`
/// write that may have landed between the last poll and exit), and
/// returns every distinct content observed, each timestamped with when
/// this reader first saw it, plus the process exit status.
fn observe_live_state(mut child: TestChild, live: &Path) -> (Vec<(Instant, String)>, i32) {
    let mut observations: Vec<(Instant, String)> = Vec::new();
    let mut last_content: Option<String> = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit_status = loop {
        if let Ok(contents) = fs::read_to_string(live) {
            assert!(
                !contents.is_empty(),
                "live-state file existed but was empty"
            );
            serde_json::from_str::<serde_json::Value>(&contents).unwrap_or_else(|e| {
                panic!("live-state file did not parse as complete JSON ({e}): {contents:?}")
            });
            if last_content.as_deref() != Some(contents.as_str()) {
                observations.push((Instant::now(), contents.clone()));
                last_content = Some(contents);
            }
        }
        if let Ok(Some(status)) = child.child.try_wait() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "electricity did not exit within the bounded 30s timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    // One more read right after exit: the final `close()` write is
    // synchronous inside the child before it exits, but may have
    // landed in the gap between this reader's last poll and
    // `try_wait` observing the exit.
    if let Ok(contents) = fs::read_to_string(live) {
        if last_content.as_deref() != Some(contents.as_str()) {
            observations.push((Instant::now(), contents));
        }
    }
    (observations, exit_status.code().unwrap_or(-1))
}

#[test]
fn mid_run_live_state_writes_land_roughly_500ms_apart_and_the_final_write_matches_out() {
    let home = TempHome::new("mid-run");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", SIX_SLEEPS_DOC);
    let live = home.path.join("live.json");
    let out = home.path.join("out.json");
    let child = spawn(&home, &config, &doc, &live, &out);

    let (observations, exit_code) = observe_live_state(child, &live);
    assert_eq!(exit_code, 0, "the run itself must succeed");

    // (c): at least two mid-run writes, i.e. at least three distinct
    // contents overall once the final one is included -- otherwise
    // this test would prove nothing about periodic writes at all.
    assert!(
        observations.len() >= 3,
        "expected at least two mid-run writes plus the final one, saw {}: {:#?}",
        observations.len(),
        observations.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    // (b): every gap between consecutive distinct contents *except the
    // last* (the final `close()` write, which `LiveStateMirror::close`
    // sends unconditionally and so may land sooner than the periodic
    // interval) must be at least ~0.5s -- a small tolerance down to
    // 0.45s for reader-thread scheduling.
    for i in 1..observations.len() - 1 {
        let gap = observations[i].0 - observations[i - 1].0;
        assert!(
            gap >= Duration::from_millis(450),
            "mid-run writes {} and {} were only {gap:?} apart, expected >= 0.45s",
            i - 1,
            i
        );
    }

    // (d): the final live-state content is byte-identical to --out's.
    let final_live = &observations.last().unwrap().1;
    let out_text = fs::read_to_string(&out).unwrap();
    assert_eq!(
        final_live, &out_text,
        "the final --live-state write must equal --out byte for byte"
    );
}
