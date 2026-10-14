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
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

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

/// Eight sequential (chain, the default) `sleep` steps of 0.3s each --
/// `~2.4s` total, long enough against `LIVE_STATE_INTERVAL`'s `500ms`
/// to see several periodic writes land roughly that far apart before
/// the run ends, and long enough that a reader thread briefly stalled
/// by a loaded runner still sees at least two mid-run versions.
const EIGHT_SLEEPS_DOC: &str = "\
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
  - name: s7
    type: tool
    provider: sleep
    params: {seconds: 0.3}
  - name: s8
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

/// Opens *path*, reads its contents from that handle, then calls
/// `metadata()` on the *same* handle (an `fstat`, not a path lookup) --
/// `write_atomic`'s write-then-rename means the content and the mtime
/// this returns always belong to the same inode, never a race between
/// this file and whatever version replaced it next.
fn read_versioned(path: &Path) -> Option<(SystemTime, String)> {
    let mut file = fs::File::open(path).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    let mtime = file.metadata().ok()?.modified().ok()?;
    Some((mtime, contents))
}

/// One mid-run `--live-state` run: polls *live* every ~10ms until the
/// child exits (plus one read right after, to catch a final `close()`
/// write that may have landed between the last poll and exit), and
/// returns every distinct content observed, each timestamped with its
/// *own* mtime (not when this reader happened to see it -- a reader
/// stall can then only make this miss a version, never shrink the gap
/// between two it did see), plus the process exit status.
fn observe_live_state(mut child: TestChild, live: &Path) -> (Vec<(SystemTime, String)>, i32) {
    let mut observations: Vec<(SystemTime, String)> = Vec::new();
    let mut last_content: Option<String> = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit_status = loop {
        if let Some((mtime, contents)) = read_versioned(live) {
            assert!(
                !contents.is_empty(),
                "live-state file existed but was empty"
            );
            serde_json::from_str::<serde_json::Value>(&contents).unwrap_or_else(|e| {
                panic!("live-state file did not parse as complete JSON ({e}): {contents:?}")
            });
            if last_content.as_deref() != Some(contents.as_str()) {
                observations.push((mtime, contents.clone()));
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
    if let Some((mtime, contents)) = read_versioned(live) {
        if last_content.as_deref() != Some(contents.as_str()) {
            observations.push((mtime, contents));
        }
    }
    (observations, exit_status.code().unwrap_or(-1))
}

#[test]
fn mid_run_live_state_writes_land_roughly_500ms_apart_and_the_final_write_matches_out() {
    let home = TempHome::new("mid-run");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", EIGHT_SLEEPS_DOC);
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

    // (b): every gap between consecutive distinct versions *except the
    // last* (the final `close()` write, which `LiveStateMirror::close`
    // sends unconditionally and so may land sooner than the periodic
    // interval) must be at least ~0.5s -- a small tolerance down to
    // 0.45s for scheduling -- measured by each write's own mtime
    // (`read_versioned`'s own doc comment), not by when this reader
    // happened to observe it, so a stalled reader can only miss a
    // version (which only makes a measured gap larger) and never
    // shrink one below the bound.
    for i in 1..observations.len() - 1 {
        let gap = observations[i]
            .0
            .duration_since(observations[i - 1].0)
            .unwrap_or(Duration::ZERO);
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

/// A two-branch `flow: tree` dynamic, one fast, one slow enough
/// (`1.5s`) to guarantee a mid-run `--live-state` write lands while
/// it's still the only one unfinished.
const TWO_BRANCH_TREE_DOC: &str = "\
effects:
  - type: dynamic
    name: lp
    flow: tree
    effects:
      - name: fast
        type: tool
        provider: sleep
        params: {seconds: 0.1}
      - name: slow
        type: tool
        provider: sleep
        params: {seconds: 1.5}
";

fn get<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    path.iter().try_fold(value, |v, key| v.get(key))
}

/// Issue #465: a mid-run `--live-state` write shows a tree's in-flight
/// branches, not only the ones that have already finished when the
/// whole tree merges. Before the fix, `prime.lp` carried no branch key
/// at all until the whole tree (`slow` included) was done -- the
/// `dynamic`'s own isolated branch nodes were unreachable from the
/// root until `Store::merge` ran -- so no mid-run write ever showed
/// `fast`'s own finished effect while `lp` itself was still running;
/// this test fails outright on an unpatched runner.
#[test]
fn mid_run_live_state_shows_a_finished_branch_before_its_tree_merges() {
    let home = TempHome::new("two-branch-tree");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", TWO_BRANCH_TREE_DOC);
    let live = home.path.join("live.json");
    let out = home.path.join("out.json");
    let child = spawn(&home, &config, &doc, &live, &out);

    let (observations, exit_code) = observe_live_state(child, &live);
    assert_eq!(exit_code, 0, "the run itself must succeed");

    let is_done = |v: &serde_json::Value, path: &[&str]| -> bool {
        get(v, path)
            .and_then(|node| node.get("meta"))
            .and_then(|meta| meta.get("completed_at"))
            .is_some_and(|c| !c.is_null())
    };
    let saw_fast_done_while_lp_still_running = observations.iter().any(|(_, contents)| {
        let parsed: serde_json::Value = serde_json::from_str(contents).unwrap();
        !is_done(&parsed, &["prime", "lp"]) && is_done(&parsed, &["prime", "lp", "fast"])
    });
    assert!(
        saw_fast_done_while_lp_still_running,
        "expected at least one mid-run write with `fast` already finished while `lp` itself \
         (the tree container, not yet merged) was still running; observed {} versions: {:#?}",
        observations.len(),
        observations.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    // The final write still matches --out, exactly like the chain-flow
    // case above -- this fix changes only the mid-run shape, never the
    // final one.
    let final_live = &observations.last().unwrap().1;
    let out_text = fs::read_to_string(&out).unwrap();
    assert_eq!(
        final_live, &out_text,
        "the final --live-state write must equal --out byte for byte"
    );
}

/// A tree *loop* (`each` with `flow: tree`) with three passes -- two
/// fast, one slow -- so a mid-run write catches at least one finished
/// `iter_<N>` pass container while the loop itself hasn't merged yet.
const TREE_LOOP_DOC: &str = "\
effects:
  - name: seconds
    type: tool
    provider: json
    params: {mode: parse, input: '[0.1, 0.1, 1.5]'}
  - type: loop
    name: draft
    flow: tree
    each:
      in: prime.seconds.value
      as: s
    body:
      - name: step
        type: tool
        provider: sleep
        params: {seconds: {from: s}}
";

/// Issue #465, second kind of tree: a tree loop's own `iter_<N>` pass
/// containers, mid-run -- the same overlay `exec::loop_::
/// run_each_tree` now registers, not only `exec::dynamic::
/// execute_tree`'s own branches. Before the fix, `prime.draft` carried
/// no `iter_<N>` key at all until the whole loop merged.
#[test]
fn mid_run_live_state_shows_a_finished_tree_loop_pass_before_it_merges() {
    let home = TempHome::new("tree-loop");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", TREE_LOOP_DOC);
    let live = home.path.join("live.json");
    let out = home.path.join("out.json");
    let child = spawn(&home, &config, &doc, &live, &out);

    let (observations, exit_code) = observe_live_state(child, &live);
    assert_eq!(exit_code, 0, "the run itself must succeed");

    let is_done = |v: &serde_json::Value, path: &[&str]| -> bool {
        get(v, path)
            .and_then(|node| node.get("meta"))
            .and_then(|meta| meta.get("completed_at"))
            .is_some_and(|c| !c.is_null())
    };
    let saw_a_pass_done_while_draft_still_running = observations.iter().any(|(_, contents)| {
        let parsed: serde_json::Value = serde_json::from_str(contents).unwrap();
        !is_done(&parsed, &["prime", "draft"])
            && (0..2)
                .any(|idx| is_done(&parsed, &["prime", "draft", &format!("iter_{idx}"), "step"]))
    });
    assert!(
        saw_a_pass_done_while_draft_still_running,
        "expected at least one mid-run write with a fast pass already finished while `draft` \
         itself (the loop container, not yet merged) was still running; observed {} versions: \
         {:#?}",
        observations.len(),
        observations.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    let final_live = &observations.last().unwrap().1;
    let out_text = fs::read_to_string(&out).unwrap();
    assert_eq!(
        final_live, &out_text,
        "the final --live-state write must equal --out byte for byte"
    );
}
