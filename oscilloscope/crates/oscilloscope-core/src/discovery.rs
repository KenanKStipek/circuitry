//! Finding the orchestrations running on this machine, from three
//! sources, most trusted first: (1) the run registry
//! ([`crate::registry`]); (2) a `ps -axo pid=,args=` scan for `cof
//! run`/`electricity` processes carrying `--events`/`--live-state`;
//! (3) a `state.live.json` modified in the last ten seconds, under
//! the current directory or one level of subdirectories. A run
//! found more than once counts once -- deduplicated first by pid,
//! then by its `--live-state` path.
//!
//! The process scan and its own command-line parsing are pure
//! functions ([`parse_ps_scan`], [`match_scanned_process`]) behind
//! two small, injectable traits ([`ProcessScanner`], [`ProcessCwd`]),
//! so a test exercises every case from a canned sample, never a real
//! process on the machine running it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::registry::{Liveness, RegistryEntry, engine_invocation, read_entries};

/// One orchestration run found by [`discover_runs`], from whichever
/// source first named it -- every field but `pid` is optional, since
/// the weakest source (a `state.live.json` found in the current
/// directory) knows only where it is, not yet what it runs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiscoveredRun {
    pub run_id: Option<String>,
    pub orchestration: Option<String>,
    pub config: Option<String>,
    pub engine: Option<String>,
    pub pid: Option<u32>,
    pub launcher: Option<String>,
    pub run_dir: Option<PathBuf>,
    pub events: Option<PathBuf>,
    pub live_state: Option<PathBuf>,
    pub started_at: Option<String>,
}

impl From<RegistryEntry> for DiscoveredRun {
    fn from(entry: RegistryEntry) -> Self {
        DiscoveredRun {
            run_id: Some(entry.run_id),
            orchestration: Some(entry.orchestration),
            config: entry.config,
            engine: Some(entry.engine),
            pid: Some(entry.pid),
            launcher: Some(entry.launcher),
            run_dir: Some(PathBuf::from(entry.run_dir)),
            events: Some(PathBuf::from(entry.events)),
            live_state: Some(PathBuf::from(entry.live_state)),
            started_at: Some(entry.started_at),
        }
    }
}

/// Every `pid`/`args` pair currently on the machine -- behind a
/// trait so a test supplies a canned sample instead of scanning the
/// real machine's own processes.
pub trait ProcessScanner {
    fn scan(&self) -> Vec<(u32, String)>;
}

/// A [`ProcessScanner`] that never finds anything -- `OSI_PROCESS_
/// SCAN=0`'s own "no process scan" (and every test that must never
/// let the real machine's own processes affect its outcome).
pub struct NullProcessScanner;

impl ProcessScanner for NullProcessScanner {
    fn scan(&self) -> Vec<(u32, String)> {
        Vec::new()
    }
}

/// [`ProcessScanner`] backed by a real `ps -ww -axo pid=,args=` call.
pub struct RealProcessScanner;

impl ProcessScanner for RealProcessScanner {
    fn scan(&self) -> Vec<(u32, String)> {
        let Ok(output) = scan_ps_command().output() else {
            return Vec::new();
        };
        parse_ps_scan(&String::from_utf8_lossy(&output.stdout))
    }
}

/// The process scan's own `ps` call, with `-ww` for the same reason
/// as [`crate::registry::liveness_ps_command`]: whole command lines,
/// whatever `$COLUMNS` says.
pub fn scan_ps_command() -> std::process::Command {
    let mut command = std::process::Command::new("ps");
    command.args(["-ww", "-axo", "pid=,args="]);
    command
}

/// Parses `ps -axo pid=,args=`'s own output into `(pid, args)` pairs
/// -- pulled out of [`RealProcessScanner`] so a test exercises it
/// from a canned sample, one of this module's own required tests
/// (issue #698).
pub fn parse_ps_scan(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid_str, rest) = line.split_once(char::is_whitespace)?;
            let pid: u32 = pid_str.parse().ok()?;
            Some((pid, rest.trim_start().to_string()))
        })
        .collect()
}

/// A scanned process's own working directory, used to resolve a
/// relative `--events`/`--live-state` path -- behind a trait since
/// not every platform exposes this at all (macOS has no `/proc`);
/// [`RealProcessCwd`] answers `None` wherever it can't, and
/// [`match_scanned_process`] then shows that path as given rather
/// than failing.
pub trait ProcessCwd {
    fn cwd_of(&self, pid: u32) -> Option<PathBuf>;
}

pub struct RealProcessCwd;

impl ProcessCwd for RealProcessCwd {
    #[cfg(target_os = "linux")]
    fn cwd_of(&self, pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    #[cfg(not(target_os = "linux"))]
    fn cwd_of(&self, _pid: u32) -> Option<PathBuf> {
        None
    }
}

fn resolve_maybe_relative(path: &str, cwd: Option<&Path>) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match cwd {
        Some(dir) => dir.join(p),
        None => p.to_path_buf(),
    }
}

/// Matches one scanned `(pid, args)` pair against `cof run`/
/// `electricity` invocations carrying `--events`/`--live-state` --
/// `None` for anything else (an unrelated process, or a
/// `cof`/`electricity` invocation with neither flag, which this scan
/// can't observe at all). A relative
/// `--events`/`--live-state`/`--out`/`--config` path is resolved
/// against `cwd` when given, else shown as-is.
///
/// `cof run <doc> [--config <cfg>] --quiet --live-state <path> --out
/// <path> [--events <path>] [-e k=v]...` and `electricity <config>
/// <doc> [-e k=v]... --out <path> [--events <path>] [--live-state
/// <path>]` (`oscilloscope_core::engine`'s own two `command`
/// builders) are the only two shapes this ever needs to parse: a
/// plain `split_whitespace` is enough for both, since neither engine
/// is ever invoked by osi with a path argument containing a space.
/// `cof` is a pip console script, so a real `cof run`'s own command
/// line names the Python interpreter first and `cof` second;
/// [`engine_invocation`] recognises that shape the same way
/// liveness does.
pub fn match_scanned_process(
    pid: u32,
    args_line: &str,
    cwd: Option<&Path>,
) -> Option<DiscoveredRun> {
    let tokens: Vec<&str> = args_line.split_whitespace().collect();
    let (bin_engine, after_bin) = engine_invocation(&tokens)?;
    let (engine, positional_start) = if bin_engine == "cof" {
        if tokens.get(after_bin) != Some(&"run") {
            return None;
        }
        ("cof", after_bin + 1)
    } else {
        ("electricity", after_bin)
    };

    let mut events: Option<&str> = None;
    let mut live_state: Option<&str> = None;
    let mut out: Option<&str> = None;
    let mut config_flag: Option<&str> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut i = positional_start;
    while i < tokens.len() {
        match tokens[i] {
            "--events" => {
                events = tokens.get(i + 1).copied();
                i += 2;
            }
            "--live-state" => {
                live_state = tokens.get(i + 1).copied();
                i += 2;
            }
            "--out" => {
                out = tokens.get(i + 1).copied();
                i += 2;
            }
            "--config" => {
                config_flag = tokens.get(i + 1).copied();
                i += 2;
            }
            "-e" => {
                i += 2;
            }
            t if t.starts_with('-') => {
                i += 1;
            }
            t => {
                positionals.push(t);
                i += 1;
            }
        }
    }

    // "carrying --events/--live-state": a cof/electricity process
    // with neither flag can't be told apart from one osi never
    // started watching at all (no observation files to find it by).
    if events.is_none() && live_state.is_none() {
        return None;
    }

    let (orchestration, config) = if engine == "cof" {
        (
            positionals.first().map(|s| s.to_string()),
            config_flag.map(str::to_string),
        )
    } else {
        // `electricity <config> <doc> ...`: the first positional is
        // the config, the second the document.
        (
            positionals.get(1).map(|s| s.to_string()),
            positionals.first().map(|s| s.to_string()),
        )
    };

    let run_dir = out
        .or(events)
        .or(live_state)
        .map(|p| resolve_maybe_relative(p, cwd))
        .and_then(|p| p.parent().map(Path::to_path_buf));

    Some(DiscoveredRun {
        run_id: None,
        orchestration,
        config,
        engine: Some(engine.to_string()),
        pid: Some(pid),
        launcher: None,
        run_dir,
        events: events.map(|p| resolve_maybe_relative(p, cwd)),
        live_state: live_state.map(|p| resolve_maybe_relative(p, cwd)),
        started_at: None,
    })
}

/// How fresh a `state.live.json` found in the current directory (or
/// one level of subdirectories) must be to count as running.
const LIVE_STATE_FRESHNESS: Duration = Duration::from_secs(10);

/// `events`'s own first line, parsed once as JSON -- `None` for a
/// missing, empty or unparseable file, or a first line that isn't
/// valid JSON at all. Reads only that one line (a `BufRead`, never
/// the whole file into memory): the first line is always `run_start`
/// when the stream has one, so nothing here ever reads any further.
fn first_event(events: &Path) -> Option<serde_json::Value> {
    use std::io::BufRead;
    let file = std::fs::File::open(events).ok()?;
    let mut first_line = String::new();
    std::io::BufReader::new(file)
        .read_line(&mut first_line)
        .ok()?;
    serde_json::from_str(first_line.trim_end()).ok()
}

/// The document and `run_id` (issue #717) `events`'s own first line
/// names, read together off one [`first_event`] call rather than two
/// separate reads of the same file.
fn orchestration_and_run_id_from_events_first_line(
    events: &Path,
) -> (Option<String>, Option<String>) {
    let Some(value) = first_event(events) else {
        return (None, None);
    };
    let orchestration = value
        .get("orchestration")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let run_id = value
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (orchestration, run_id)
}

/// `run_id`, from `state.live.json`'s own top-level `_run_id` (both
/// engines write it, `cli/runtime_shim.py`'s own `state["_run_id"]`)
/// -- the fallback issue #717 names for a run found with no
/// `--events` stream of its own at all (`osi watch` of a bare
/// `state.live.json`).
fn run_id_from_state_file(live_state: &Path) -> Option<String> {
    let bytes = std::fs::read(live_state).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("_run_id")?.as_str().map(str::to_string)
}

/// `run_id` (issue #717), events' own first line preferred over
/// `state.live.json`'s `_run_id` -- either, both or neither of
/// `events`/`live_state` may be known, and this never fails just
/// because one is absent or unreadable.
fn discovered_run_id(events: Option<&Path>, live_state: Option<&Path>) -> Option<String> {
    events
        .and_then(|e| orchestration_and_run_id_from_events_first_line(e).1)
        .or_else(|| live_state.and_then(run_id_from_state_file))
}

fn live_state_run_if_fresh(dir: &Path, now: SystemTime) -> Option<DiscoveredRun> {
    let live_state = dir.join("state.live.json");
    let modified = std::fs::metadata(&live_state).ok()?.modified().ok()?;
    let age = now.duration_since(modified).ok()?;
    if age > LIVE_STATE_FRESHNESS {
        return None;
    }
    let events = dir.join("events.jsonl");
    let events = events.exists().then_some(events);
    let (orchestration, run_id) = match events.as_deref() {
        Some(e) => orchestration_and_run_id_from_events_first_line(e),
        None => (None, None),
    };
    let run_id = run_id.or_else(|| run_id_from_state_file(&live_state));
    Some(DiscoveredRun {
        run_id,
        run_dir: Some(dir.to_path_buf()),
        live_state: Some(live_state),
        events,
        orchestration,
        ..Default::default()
    })
}

/// Scans `cwd` and one level of its subdirectories for a
/// `state.live.json` modified within [`LIVE_STATE_FRESHNESS`] of
/// `now` (the third discovery source) -- `now` is the caller's own,
/// so a test fixes it rather than racing the real clock.
pub fn scan_live_state_in_cwd(cwd: &Path, now: SystemTime) -> Vec<DiscoveredRun> {
    let mut found = Vec::new();
    if let Some(run) = live_state_run_if_fresh(cwd, now) {
        found.push(run);
    }
    if let Ok(read_dir) = std::fs::read_dir(cwd) {
        for item in read_dir.flatten() {
            let path = item.path();
            if path.is_dir() {
                if let Some(run) = live_state_run_if_fresh(&path, now) {
                    found.push(run);
                }
            }
        }
    }
    found
}

/// One plain line per running run (bare `osi` without a TTY, or
/// with `--log`, prints one of these per [`discover_runs`] result
/// and exits 0) -- document, its own short name (issue #717,
/// `—` before its `run_start` has arrived), config, engine, pid, run
/// directory, `"—"` for anything not known. `name` is the caller's
/// own, already [`crate::names::disambiguated_names`]'d against
/// every other run in the same listing.
pub fn format_plain_line(run: &DiscoveredRun, name: Option<&str>) -> String {
    let document = run.orchestration.as_deref().unwrap_or("—");
    let name = name.unwrap_or("—");
    let config = run.config.as_deref().unwrap_or("—");
    let engine = run.engine.as_deref().unwrap_or("—");
    let pid = run
        .pid
        .map(|p| p.to_string())
        .unwrap_or_else(|| "—".to_string());
    let run_dir = run
        .run_dir
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_string());
    format!("{document}  {name}  {config}  {engine}  pid {pid}  {run_dir}")
}

/// Merges the registry, a process scan and a `state.live.json` scan
/// of the current directory into one deduplicated list: a run found
/// twice counts once (pid, then its `--live-state` path), and the
/// result is ordered by `started_at` (oldest -- i.e. earliest launch
/// -- first), runs with no known `started_at` (a bare scan match, or
/// the cwd source) ordered after every run that has one. `cwd` is
/// `None` to skip the third source entirely (`OSI_PROCESS_SCAN=0`'s
/// own "no process scan, no current-directory source"); `scanner`
/// answering an empty scan already does the same for the second.
#[allow(clippy::too_many_arguments)]
pub fn discover_runs(
    registry_dir: Option<&Path>,
    liveness: &dyn Liveness,
    scanner: &dyn ProcessScanner,
    process_cwd: &dyn ProcessCwd,
    cwd: Option<&Path>,
    now: SystemTime,
) -> Vec<DiscoveredRun> {
    let mut runs: Vec<DiscoveredRun> = Vec::new();
    if let Some(dir) = registry_dir {
        runs.extend(
            read_entries(dir, liveness)
                .into_iter()
                .map(DiscoveredRun::from),
        );
    }

    for (pid, args_line) in scanner.scan() {
        if runs.iter().any(|r| r.pid == Some(pid)) {
            continue;
        }
        let Some(mut discovered) =
            match_scanned_process(pid, &args_line, process_cwd.cwd_of(pid).as_deref())
        else {
            continue;
        };
        if discovered.live_state.is_some()
            && runs.iter().any(|r| r.live_state == discovered.live_state)
        {
            continue;
        }
        // `match_scanned_process` is a pure function (never touches
        // the filesystem, so a test exercises it from a canned
        // command line alone) and so never learns `run_id` from the
        // command line itself -- backfilled here, the one place this
        // source is allowed to read the events/live-state files its
        // own match already pointed at.
        discovered.run_id = discovered_run_id(
            discovered.events.as_deref(),
            discovered.live_state.as_deref(),
        );
        runs.push(discovered);
    }

    for discovered in cwd
        .map(|cwd| scan_live_state_in_cwd(cwd, now))
        .unwrap_or_default()
    {
        if runs.iter().any(|r| r.live_state == discovered.live_state) {
            continue;
        }
        runs.push(discovered);
    }

    runs.sort_by(|a, b| match (&a.started_at, &b.started_at) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryEntry;
    use std::time::Duration;

    #[test]
    fn the_scan_ps_call_asks_for_whole_command_lines() {
        let command = scan_ps_command();
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-ww", "-axo", "pid=,args="]);
    }

    struct AllAlive;
    impl Liveness for AllAlive {
        fn is_alive(&self, _pid: u32) -> bool {
            true
        }
    }

    struct NoScan;
    impl ProcessScanner for NoScan {
        fn scan(&self) -> Vec<(u32, String)> {
            Vec::new()
        }
    }

    struct FixedScan(Vec<(u32, String)>);
    impl ProcessScanner for FixedScan {
        fn scan(&self) -> Vec<(u32, String)> {
            self.0.clone()
        }
    }

    struct NoCwd;
    impl ProcessCwd for NoCwd {
        fn cwd_of(&self, _pid: u32) -> Option<PathBuf> {
            None
        }
    }

    struct FixedCwd(PathBuf);
    impl ProcessCwd for FixedCwd {
        fn cwd_of(&self, _pid: u32) -> Option<PathBuf> {
            Some(self.0.clone())
        }
    }

    #[test]
    fn format_plain_line_shows_an_em_dash_for_anything_unknown() {
        let run = DiscoveredRun {
            pid: Some(42),
            run_dir: Some(PathBuf::from("/work/run-dir")),
            ..Default::default()
        };
        assert_eq!(
            format_plain_line(&run, None),
            "—  —  —  —  pid 42  /work/run-dir"
        );
    }

    #[test]
    fn format_plain_line_shows_every_known_field() {
        let run = DiscoveredRun {
            orchestration: Some("/work/doc.yml".to_string()),
            config: Some("/work/cfg.json".to_string()),
            engine: Some("cof".to_string()),
            pid: Some(42),
            run_dir: Some(PathBuf::from("/work/run-dir")),
            ..Default::default()
        };
        assert_eq!(
            format_plain_line(&run, Some("amber-falcon")),
            "/work/doc.yml  amber-falcon  /work/cfg.json  cof  pid 42  /work/run-dir"
        );
    }

    #[test]
    fn parse_ps_scan_reads_pid_and_args() {
        let sample = "  123 cof run doc.yml --live-state /tmp/a/state.live.json\n\
                        456  electricity /tmp/cfg.json doc.yml --out /tmp/b/state.json\n";
        let parsed = parse_ps_scan(sample);
        assert_eq!(
            parsed,
            vec![
                (
                    123,
                    "cof run doc.yml --live-state /tmp/a/state.live.json".to_string()
                ),
                (
                    456,
                    "electricity /tmp/cfg.json doc.yml --out /tmp/b/state.json".to_string()
                ),
            ]
        );
    }

    #[test]
    fn parse_ps_scan_skips_unparseable_lines() {
        assert_eq!(parse_ps_scan("not-a-pid some args\n"), Vec::new());
        assert_eq!(parse_ps_scan(""), Vec::new());
    }

    #[test]
    fn match_scanned_process_reads_a_cof_run_line() {
        let run = match_scanned_process(
            123,
            "/usr/local/bin/cof run /work/doc.yml --config /work/cfg.json --quiet \
             --live-state /work/run-dir/state.live.json --out /work/run-dir/state.json \
             --events /work/run-dir/events.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(run.engine, Some("cof".to_string()));
        assert_eq!(run.pid, Some(123));
        assert_eq!(run.orchestration, Some("/work/doc.yml".to_string()));
        assert_eq!(run.config, Some("/work/cfg.json".to_string()));
        assert_eq!(run.run_dir, Some(PathBuf::from("/work/run-dir")));
        assert_eq!(
            run.events,
            Some(PathBuf::from("/work/run-dir/events.jsonl"))
        );
        assert_eq!(
            run.live_state,
            Some(PathBuf::from("/work/run-dir/state.live.json"))
        );
    }

    #[test]
    fn match_scanned_process_reads_a_cof_run_line_through_its_own_interpreter() {
        // `cof` is a pip console script: a real `ps` report names the
        // Python interpreter first and `cof` second.
        let run = match_scanned_process(
            123,
            "/x/.venv/bin/python /x/.venv/bin/cof run /work/doc.yml \
             --live-state /work/run-dir/state.live.json",
            None,
        )
        .unwrap();
        assert_eq!(run.engine, Some("cof".to_string()));
        assert_eq!(run.orchestration, Some("/work/doc.yml".to_string()));
    }

    #[test]
    fn match_scanned_process_reads_an_electricity_line_config_then_doc() {
        let run = match_scanned_process(
            456,
            "electricity /work/cfg.json /work/doc.yml --out /work/run-dir/state.json \
             --events /work/run-dir/events.jsonl --live-state /work/run-dir/state.live.json",
            None,
        )
        .unwrap();
        assert_eq!(run.engine, Some("electricity".to_string()));
        assert_eq!(run.orchestration, Some("/work/doc.yml".to_string()));
        assert_eq!(run.config, Some("/work/cfg.json".to_string()));
    }

    #[test]
    fn match_scanned_process_ignores_unrelated_processes() {
        assert!(match_scanned_process(1, "python3 something.py", None).is_none());
    }

    #[test]
    fn match_scanned_process_ignores_a_cof_or_electricity_run_with_neither_flag() {
        assert!(match_scanned_process(1, "cof run doc.yml", None).is_none());
    }

    #[test]
    fn match_scanned_process_resolves_a_relative_path_against_the_process_cwd() {
        let run = match_scanned_process(
            1,
            "cof run doc.yml --live-state run-dir/state.live.json",
            Some(Path::new("/work")),
        )
        .unwrap();
        assert_eq!(
            run.live_state,
            Some(PathBuf::from("/work/run-dir/state.live.json"))
        );
    }

    #[test]
    fn match_scanned_process_shows_a_relative_path_as_given_with_no_cwd() {
        let run = match_scanned_process(
            1,
            "cof run doc.yml --live-state run-dir/state.live.json",
            None,
        )
        .unwrap();
        assert_eq!(
            run.live_state,
            Some(PathBuf::from("run-dir/state.live.json"))
        );
    }

    #[test]
    fn scan_live_state_in_cwd_finds_a_fresh_file_and_skips_a_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let fresh_dir = dir.path().join("fresh");
        let stale_dir = dir.path().join("stale");
        std::fs::create_dir(&fresh_dir).unwrap();
        std::fs::create_dir(&stale_dir).unwrap();
        std::fs::write(fresh_dir.join("state.live.json"), b"{}").unwrap();
        let stale_path = stale_dir.join("state.live.json");
        std::fs::write(&stale_path, b"{}").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        let file = std::fs::File::open(&stale_path).unwrap();
        file.set_modified(old).unwrap();

        let found = scan_live_state_in_cwd(dir.path(), std::time::SystemTime::now());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].run_dir, Some(fresh_dir));
    }

    #[test]
    fn scan_live_state_in_cwd_names_the_document_from_the_events_first_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.live.json"), b"{}").unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"ts\":\"2026-01-01T00:00:00Z\",\"ev\":\"run_start\",\"run_id\":\"r1\",\"orchestration\":\"do.yml\"}\n",
        )
        .unwrap();

        let found = scan_live_state_in_cwd(dir.path(), std::time::SystemTime::now());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].orchestration, Some("do.yml".to_string()));
    }

    #[test]
    fn scan_live_state_in_cwd_leaves_the_document_unknown_with_no_events_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.live.json"), b"{}").unwrap();

        let found = scan_live_state_in_cwd(dir.path(), std::time::SystemTime::now());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].orchestration, None);
    }

    #[test]
    fn discover_runs_dedups_a_registry_entry_also_found_by_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let entry = RegistryEntry {
            run_id: "run-1".to_string(),
            orchestration: "/work/doc.yml".to_string(),
            config: None,
            engine: "cof".to_string(),
            pid: 123,
            launcher: "osi 0.1.0".to_string(),
            run_dir: "/work/run-dir".to_string(),
            events: "/work/run-dir/events.jsonl".to_string(),
            live_state: "/work/run-dir/state.live.json".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };
        crate::registry::write_entry(dir.path(), &entry).unwrap();

        let scan = FixedScan(vec![(
            123,
            "cof run /work/doc.yml --live-state /work/run-dir/state.live.json".to_string(),
        )]);
        let empty_cwd = tempfile::tempdir().unwrap();
        let runs = discover_runs(
            Some(dir.path()),
            &AllAlive,
            &scan,
            &NoCwd,
            Some(empty_cwd.path()),
            std::time::SystemTime::now(),
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, Some("run-1".to_string()));
    }

    #[test]
    fn discover_runs_dedups_two_scanned_processes_sharing_a_live_state_path() {
        // Same `--live-state` path, different pid -- the registry
        // key (pid) can't tell these apart, but the live-state path
        // can (issue #698's "a run found twice counts once (pid,
        // then the `--live-state` path)").
        let scan = FixedScan(vec![
            (
                1,
                "cof run doc.yml --live-state /work/run-dir/state.live.json".to_string(),
            ),
            (
                2,
                "cof run doc.yml --live-state /work/run-dir/state.live.json".to_string(),
            ),
        ]);
        let empty_cwd = tempfile::tempdir().unwrap();
        let runs = discover_runs(
            None,
            &AllAlive,
            &scan,
            &NoCwd,
            Some(empty_cwd.path()),
            std::time::SystemTime::now(),
        );
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn discover_runs_dedups_a_scanned_process_also_found_by_the_cwd_scan() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(cwd.path().join("state.live.json"), b"{}").unwrap();
        let live_state = cwd.path().join("state.live.json");
        let scan = FixedScan(vec![(
            1,
            format!("cof run doc.yml --live-state {}", live_state.display()),
        )]);
        let runs = discover_runs(
            None,
            &AllAlive,
            &scan,
            &NoCwd,
            Some(cwd.path()),
            std::time::SystemTime::now(),
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].pid, Some(1));
    }

    #[test]
    fn discover_runs_orders_by_started_at_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let later = RegistryEntry {
            started_at: "2026-01-01T00:00:05Z".to_string(),
            ..sample_entry_for_order("later", 2)
        };
        let earlier = RegistryEntry {
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ..sample_entry_for_order("earlier", 1)
        };
        crate::registry::write_entry(dir.path(), &later).unwrap();
        crate::registry::write_entry(dir.path(), &earlier).unwrap();

        let empty_cwd = tempfile::tempdir().unwrap();
        let runs = discover_runs(
            Some(dir.path()),
            &AllAlive,
            &NoScan,
            &NoCwd,
            Some(empty_cwd.path()),
            std::time::SystemTime::now(),
        );
        assert_eq!(
            runs.iter().map(|r| r.run_id.clone()).collect::<Vec<_>>(),
            vec![Some("earlier".to_string()), Some("later".to_string())]
        );
    }

    fn sample_entry_for_order(run_id: &str, pid: u32) -> RegistryEntry {
        RegistryEntry {
            run_id: run_id.to_string(),
            orchestration: "/work/doc.yml".to_string(),
            config: None,
            engine: "cof".to_string(),
            pid,
            launcher: "osi 0.1.0".to_string(),
            run_dir: "/work/run-dir".to_string(),
            events: "/work/run-dir/events.jsonl".to_string(),
            live_state: format!("/work/run-dir-{pid}/state.live.json"),
            started_at: String::new(),
        }
    }

    #[test]
    fn discover_runs_orders_a_run_with_no_started_at_after_those_with_one() {
        let scan = FixedScan(vec![(
            1,
            "cof run doc.yml --live-state /work/run-dir/state.live.json".to_string(),
        )]);
        let dir = tempfile::tempdir().unwrap();
        crate::registry::write_entry(
            dir.path(),
            &RegistryEntry {
                started_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_entry_for_order("known", 2)
            },
        )
        .unwrap();
        let empty_cwd = tempfile::tempdir().unwrap();
        let runs = discover_runs(
            Some(dir.path()),
            &AllAlive,
            &scan,
            &NoCwd,
            Some(empty_cwd.path()),
            std::time::SystemTime::now(),
        );
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, Some("known".to_string()));
        assert_eq!(runs[1].started_at, None);
    }

    #[test]
    fn process_cwd_trait_is_consulted_for_a_relative_path() {
        let scan = FixedScan(vec![(
            1,
            "cof run doc.yml --live-state run-dir/state.live.json".to_string(),
        )]);
        let empty_cwd = tempfile::tempdir().unwrap();
        let runs = discover_runs(
            None,
            &AllAlive,
            &scan,
            &FixedCwd(PathBuf::from("/work")),
            Some(empty_cwd.path()),
            std::time::SystemTime::now(),
        );
        assert_eq!(
            runs[0].live_state,
            Some(PathBuf::from("/work/run-dir/state.live.json"))
        );
    }
}
