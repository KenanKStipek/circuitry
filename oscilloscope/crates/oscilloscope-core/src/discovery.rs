//! Finding the orchestrations running on this machine (issue #698's
//! "Settled 2"), from three sources, most trusted first: (1) the run
//! registry ([`crate::registry`]); (2) a `ps -axo pid=,args=` scan
//! for `cof run`/`electricity` processes carrying `--events`/
//! `--live-state`; (3) a `state.live.json` modified in the last ten
//! seconds, under the current directory or one level of
//! subdirectories. A run found more than once counts once --
//! deduplicated first by pid, then by its `--live-state` path.
//!
//! The process scan and its own command-line parsing are pure
//! functions ([`parse_ps_scan`], [`match_scanned_process`]) behind
//! two small, injectable traits ([`ProcessScanner`], [`ProcessCwd`]),
//! so a test exercises every case from a canned sample, never a real
//! process on the machine running it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::registry::{Liveness, RegistryEntry, read_entries};

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

/// Every `pid`/`args` pair currently on the machine (issue #698's
/// "Settled 2") -- behind a trait so a test supplies a canned sample
/// instead of scanning the real machine's own processes.
pub trait ProcessScanner {
    fn scan(&self) -> Vec<(u32, String)>;
}

/// [`ProcessScanner`] backed by a real `ps -axo pid=,args=` call.
pub struct RealProcessScanner;

impl ProcessScanner for RealProcessScanner {
    fn scan(&self) -> Vec<(u32, String)> {
        let Ok(output) = std::process::Command::new("ps")
            .args(["-axo", "pid=,args="])
            .output()
        else {
            return Vec::new();
        };
        parse_ps_scan(&String::from_utf8_lossy(&output.stdout))
    }
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
/// relative `--events`/`--live-state` path (issue #698's "Settled
/// 2") -- behind a trait since not every platform exposes this at
/// all (macOS has no `/proc`); [`RealProcessCwd`] answers `None`
/// wherever it can't, and [`match_scanned_process`] then shows that
/// path as given rather than failing.
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
/// `electricity` invocations carrying `--events`/`--live-state`
/// (issue #698's "Settled 2") -- `None` for anything else (an
/// unrelated process, or a `cof`/`electricity` invocation with
/// neither flag, which this scan can't observe at all). A relative
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
pub fn match_scanned_process(
    pid: u32,
    args_line: &str,
    cwd: Option<&Path>,
) -> Option<DiscoveredRun> {
    let tokens: Vec<&str> = args_line.split_whitespace().collect();
    let bin = Path::new(tokens.first()?)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let (engine, positional_start) = if bin == "cof" {
        if tokens.get(1) != Some(&"run") {
            return None;
        }
        ("cof", 2)
    } else if bin == "electricity" {
        ("electricity", 1)
    } else {
        return None;
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
/// one level of subdirectories) must be to count as running (issue
/// #698's "Settled 2").
const LIVE_STATE_FRESHNESS: Duration = Duration::from_secs(10);

fn live_state_run_if_fresh(dir: &Path, now: SystemTime) -> Option<DiscoveredRun> {
    let live_state = dir.join("state.live.json");
    let modified = std::fs::metadata(&live_state).ok()?.modified().ok()?;
    let age = now.duration_since(modified).ok()?;
    if age > LIVE_STATE_FRESHNESS {
        return None;
    }
    let events = dir.join("events.jsonl");
    Some(DiscoveredRun {
        run_dir: Some(dir.to_path_buf()),
        live_state: Some(live_state),
        events: events.exists().then_some(events),
        ..Default::default()
    })
}

/// Scans `cwd` and one level of its subdirectories for a
/// `state.live.json` modified within [`LIVE_STATE_FRESHNESS`] of
/// `now` (issue #698's "Settled 2", third source) -- `now` is the
/// caller's own, so a test fixes it rather than racing the real
/// clock.
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

/// One plain line per running run (issue #698's "Settled 2": bare
/// `osi` without a TTY, or with `--log`, prints one of these per
/// [`discover_runs`] result and exits 0) -- document, config,
/// engine, pid, run directory, `"—"` for anything not known.
pub fn format_plain_line(run: &DiscoveredRun) -> String {
    let document = run.orchestration.as_deref().unwrap_or("—");
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
    format!("{document}  {config}  {engine}  pid {pid}  {run_dir}")
}

/// Merges the registry, a process scan and a `state.live.json` scan
/// of the current directory into one deduplicated list (issue #698's
/// "Settled 2" and "Settled 3"'s launch-time ordering): a run found
/// twice counts once (pid, then its `--live-state` path), and the
/// result is ordered by `started_at` (oldest -- i.e. earliest launch
/// -- first), runs with no known `started_at` (a bare scan match, or
/// the cwd source) ordered after every run that has one.
#[allow(clippy::too_many_arguments)]
pub fn discover_runs(
    registry_dir: Option<&Path>,
    liveness: &dyn Liveness,
    scanner: &dyn ProcessScanner,
    process_cwd: &dyn ProcessCwd,
    cwd: &Path,
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
        let Some(discovered) =
            match_scanned_process(pid, &args_line, process_cwd.cwd_of(pid).as_deref())
        else {
            continue;
        };
        if discovered.live_state.is_some()
            && runs.iter().any(|r| r.live_state == discovered.live_state)
        {
            continue;
        }
        runs.push(discovered);
    }

    for discovered in scan_live_state_in_cwd(cwd, now) {
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
        assert_eq!(format_plain_line(&run), "—  —  —  pid 42  /work/run-dir");
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
            format_plain_line(&run),
            "/work/doc.yml  /work/cfg.json  cof  pid 42  /work/run-dir"
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
            empty_cwd.path(),
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
            empty_cwd.path(),
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
            cwd.path(),
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
            empty_cwd.path(),
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
            empty_cwd.path(),
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
            empty_cwd.path(),
            std::time::SystemTime::now(),
        );
        assert_eq!(
            runs[0].live_state,
            Some(PathBuf::from("/work/run-dir/state.live.json"))
        );
    }
}
