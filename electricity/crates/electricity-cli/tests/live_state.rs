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
    let is_started = |v: &serde_json::Value, path: &[&str]| -> bool {
        get(v, path)
            .and_then(|node| node.get("meta"))
            .and_then(|meta| meta.get("created_at"))
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

    // PR #549's review: electricity's own overlay shows a
    // branch's *in-progress* node too, as soon as it starts -- unlike
    // cof's own publisher, which republishes a branch only once that
    // branch itself has called `on_write` at least once (a `prompt`/
    // `tool` effect never calls it on attempt start, only once it has
    // a `waiting_for` or a result to report). This is a deliberate,
    // documented difference (`electricity/DESIGN.md` §13, Q11), not a
    // bug to hide: copying cof's staler view would mean taking a
    // branch snapshot on every write instead of lazily at publish
    // time, which issue #465's own performance ruling forbids. Assert
    // it directly, on the real mid-run shape, rather than merely
    // asserting the shape isn't broken.
    let saw_slow_started_but_not_done_while_lp_still_running =
        observations.iter().any(|(_, contents)| {
            let parsed: serde_json::Value = serde_json::from_str(contents).unwrap();
            !is_done(&parsed, &["prime", "lp"])
                && is_started(&parsed, &["prime", "lp", "slow"])
                && !is_done(&parsed, &["prime", "lp", "slow"])
        });
    assert!(
        saw_slow_started_but_not_done_while_lp_still_running,
        "expected at least one mid-run write with `slow` already started (but not finished) \
         while `lp` itself was still running -- electricity's own overlay shows a started \
         branch, unlike cof's; observed {} versions: {:#?}",
        observations.len(),
        observations.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    // The real published shape, key order included: the container's
    // own skeleton `value`/`meta` first (written before the tree ever
    // dispatches), `fast` appended once it writes (branch 0), `slow`
    // appended once it writes too (branch 1) -- `node.update`'s own
    // ascending-index rule, `core/store/store.py::Store.
    // parallel_branches`. Checked with this crate's own ordered JSON
    // reader (`electricity_value::Value::Dict` is an `IndexMap`), not
    // `serde_json` -- enabling `serde_json`'s own `preserve_order`
    // feature here instead would pull every crate's own test build
    // onto `indexmap`'s `serde_json::Value::Object`, changing stack
    // frame sizes workspace-wide and overflowing `electricity-
    // compiler`'s own `depth_safety.rs` 2 MiB-thread tests (PR #492).
    let lp_key_order_once_both_branches_have_written =
        observations.iter().find_map(|(_, contents)| {
            let parsed = electricity_json::loads(contents).unwrap();
            let lp = parsed
                .as_dict()
                .and_then(|d| d.get(&electricity_value::Value::Str("prime".to_string())))
                .and_then(electricity_value::Value::as_dict)
                .and_then(|d| d.get(&electricity_value::Value::Str("lp".to_string())))
                .and_then(electricity_value::Value::as_dict)?;
            let has = |k: &str| lp.contains_key(&electricity_value::Value::Str(k.to_string()));
            if has("fast") && has("slow") {
                Some(
                    lp.keys()
                        .map(|k| match k {
                            electricity_value::Value::Str(s) => s.clone(),
                            other => panic!("expected a string key, got {other:?}"),
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        });
    assert_eq!(
        lp_key_order_once_both_branches_have_written,
        Some(vec![
            "value".to_string(),
            "meta".to_string(),
            "fast".to_string(),
            "slow".to_string(),
        ]),
        "expected a mid-run write with both `fast` and `slow` present in exactly this key \
         order, observed {} versions: {:#?}",
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

/// A `use: {inline: ...}` child whose own document has a top-level
/// `flow: tree` (two branches, one fast, one slow). The child frame
/// maps the child's root to the `use` node's own path -- an
/// unnamed/transparent root tree appends no name segment of its own --
/// so the child's own tree registers its branches at the *same* path
/// `ChildObserver::write` periodically grafts its own snapshot onto.
const USE_CHILD_TOP_LEVEL_TREE_DOC: &str = "\
effects:\n  - type: use\n    name: step\n    inline: |\n      flow: tree\n      effects:\n        - name: fast\n          type: tool\n          provider: sleep\n          params: {seconds: 0.1}\n        - name: slow\n          type: tool\n          provider: sleep\n          params: {seconds: 1.5}\n";

/// PR #549's review: a `use` child whose own document has a
/// top-level `flow: tree` collides its own tree overlay with its own
/// `ChildObserver` graft at the exact same path. Before this fix,
/// a single `BTreeMap<String, GraftEntry>` meant whichever of the
/// two registered *second* at that path silently destroyed the first
/// -- once the child wrote anything after its own tree had already
/// registered its branches, the tree overlay vanished for the rest of
/// the run, issue #465's own original bug reintroduced through a
/// `use` child.
#[test]
fn mid_run_live_state_keeps_a_use_childs_own_tree_overlay_through_a_later_child_write() {
    let home = TempHome::new("use-child-top-level-tree");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", USE_CHILD_TOP_LEVEL_TREE_DOC);
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
    let has_fast = |v: &serde_json::Value| -> bool { get(v, &["prime", "step", "fast"]).is_some() };

    let parsed_observations: Vec<serde_json::Value> = observations
        .iter()
        .map(|(_, c)| serde_json::from_str(c).unwrap())
        .collect();
    let still_running: Vec<&serde_json::Value> = parsed_observations
        .iter()
        .filter(|parsed| !is_done(parsed, &["prime", "step"]))
        .collect();

    assert!(
        still_running.iter().any(|v| has_fast(v)),
        "expected at least one mid-run write with `fast` visible under the use child's own \
         node while the child (mapped to the same path as its own graft) was still running; \
         observed {} versions: {:#?}",
        observations.len(),
        observations.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );
    assert!(
        still_running.last().is_some_and(|v| has_fast(v)),
        "the tree overlay must survive every later child write at the same path -- `fast` \
         must still be visible in the *last* mid-run write before the use child merges, not \
         only the first one right after it finished; observed {} versions: {:#?}",
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
