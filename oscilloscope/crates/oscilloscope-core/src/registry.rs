//! osi's run registry (issue #698's "Settled 1"): one `0600` JSON
//! file per run this `osi` launched or watched, in a `0700`
//! directory under `$XDG_RUNTIME_DIR/circuitry/osi/runs/`, else
//! `$XDG_STATE_HOME/circuitry/osi/runs/`, else
//! `~/.local/state/circuitry/osi/runs/` -- `OSI_RUNS_DIR` overrides
//! every one of those for tests, the same convention
//! [`crate::history`]'s own `OSI_HISTORY_DIR` already follows.
//!
//! An entry is named by its own `run_id` and carries exactly the
//! fields `oscilloscope/design/tui-design.md` §7.2 lists: `run_id`,
//! `orchestration`, `config`, `engine`, `pid`, `launcher`, `run_dir`,
//! `events`, `live_state`, `started_at`. Callers never touch the file
//! format directly: [`write_entry`], [`remove_entry`] and
//! [`read_entries`] are the only entry points that open a file, and
//! all three take the already-resolved registry directory -- `env` is
//! read exactly once, by the CLI binary, the same convention
//! `history`/`config_discovery` already follow.
//!
//! A pid alone can be fooled by a recycled pid; [`read_entries`]
//! prunes an entry whose pid is dead *or* whose command line no
//! longer names `cof`/`electricity` (`ps -o args= -p <pid>`), through
//! the injectable [`Liveness`] trait -- no test ever checks a fixture
//! pid against the real machine.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One running `osi`-launched or `osi watch`ed run (DESIGN.md's own
/// registry section; `tui-design.md` §7.2). `orchestration`/`config`
/// are absolute paths (the caller resolves them before building this
/// -- this module never touches the filesystem to do so).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub run_id: String,
    pub orchestration: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub config: Option<String>,
    pub engine: String,
    pub pid: u32,
    /// `"osi <version>"` or `"osi <version> watch"` -- which of the
    /// two wrote this entry.
    pub launcher: String,
    pub run_dir: String,
    pub events: String,
    pub live_state: String,
    pub started_at: String,
}

/// Whether a pid named in a registry entry (or found by a process
/// scan) is still a real, running `cof`/`electricity` -- behind a
/// trait so a test injects the answer rather than ever checking a
/// fixture's pid against the real machine (issue #698's "Settled 1").
pub trait Liveness {
    fn is_alive(&self, pid: u32) -> bool;
}

/// [`Liveness`] backed by a real `ps -o args= -p <pid>` call: alive
/// means the pid exists *and* its command line still names `cof` or
/// `electricity` -- a bare `kill(pid, 0)` can be fooled by a recycled
/// pid, but `ps` always reports whatever process actually holds that
/// pid right now, recycled or not.
pub struct RealLiveness;

impl Liveness for RealLiveness {
    fn is_alive(&self, pid: u32) -> bool {
        let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "args=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        command_line_names_engine(&String::from_utf8_lossy(&output.stdout))
    }
}

/// Whether a `ps`-reported command line's own first token names
/// `cof` or `electricity` -- pulled out of [`RealLiveness`] so a test
/// exercises every case (a bare name, a full path, an unrelated
/// command) without a real pid at all.
pub fn command_line_names_engine(args_line: &str) -> bool {
    let Some(first) = args_line.split_whitespace().next() else {
        return false;
    };
    let base = Path::new(first)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(first);
    base == "cof" || base == "electricity"
}

/// `OSI_RUNS_DIR`, else `$XDG_RUNTIME_DIR/circuitry/osi/runs`, else
/// `$XDG_STATE_HOME/circuitry/osi/runs`, else
/// `~/.local/state/circuitry/osi/runs` (issue #698's "Settled 1") --
/// every parameter is the caller's own, already-read value (`env` is
/// read exactly once, by the binary crate); `None` only when neither
/// an override nor `$HOME` is available at all. macOS has no
/// `XDG_RUNTIME_DIR`, so the `$XDG_STATE_HOME`/`$HOME` fallback is the
/// common case there, keeping one `circuitry/osi` root next to
/// [`crate::history`]'s own directory.
pub fn resolve_runs_dir(
    override_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    xdg_state_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        if !dir.as_os_str().is_empty() {
            return Some(dir.to_path_buf());
        }
    }
    if let Some(dir) = xdg_runtime_dir {
        if !dir.as_os_str().is_empty() {
            return Some(dir.join("circuitry").join("osi").join("runs"));
        }
    }
    let state_home = match xdg_state_home {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => home?.join(".local").join("state"),
    };
    Some(state_home.join("circuitry").join("osi").join("runs"))
}

/// Creates `dir` with mode `0700` when it doesn't exist yet -- the
/// same rule [`crate::history::ensure_history_dir`] already follows,
/// including its own warning (never a failure) for an existing
/// directory that is group- or world-accessible.
pub fn ensure_runs_dir(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        let mode = fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            let _ = writeln!(
                std::io::stderr(),
                "osi: run registry directory {} is group- or world-accessible (mode {:o}); \
                 it can hold document and config paths",
                dir.display(),
                mode & 0o777
            );
        }
        return Ok(());
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(dir)
}

/// The registry file for `run_id`.
pub fn registry_file_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!("{run_id}.json"))
}

/// Writes `entry`'s own file, creating the registry directory first
/// if it doesn't exist yet. Written through a temp file in the same
/// directory and renamed into place, so a reader (this process' own
/// [`read_entries`], or a concurrent one) never sees a half-written
/// file.
pub fn write_entry(dir: &Path, entry: &RegistryEntry) -> std::io::Result<()> {
    ensure_runs_dir(dir)?;
    let path = registry_file_path(dir, &entry.run_id);
    let tmp = dir.join(format!("{}.json.tmp", entry.run_id));
    let json = serde_json::to_vec_pretty(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)
}

/// Removes `run_id`'s own file, if it exists -- a no-op, not an
/// error, when it's already gone (the run's own end, a detach and
/// osi's own exit can all race to remove the same entry).
pub fn remove_entry(dir: &Path, run_id: &str) -> std::io::Result<()> {
    match fs::remove_file(registry_file_path(dir, run_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Every entry in `dir` whose pid is still alive (per `liveness`),
/// oldest `started_at` first -- a corrupt, partial or otherwise
/// unparseable file is skipped, not an error, the same tolerance
/// [`crate::history::read_records`] already gives a bad line. An
/// entry whose pid is *not* alive is removed from disk right here
/// (issue #698's "pruned on read when its pid is dead"), not just
/// left out of the returned list.
pub fn read_entries(dir: &Path, liveness: &dyn Liveness) -> Vec<RegistryEntry> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in read_dir.flatten() {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(entry) = serde_json::from_slice::<RegistryEntry>(&bytes) else {
            continue;
        };
        if liveness.is_alive(entry.pid) {
            entries.push(entry);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
    entries.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    entries
}

/// The config shown for an attached run (issue #698's "Settled 8"):
/// the registry entry's own `config`, else `run_start`'s (the engine
/// half, issue #697), else whatever the process's own arguments gave,
/// else `None` -- every source is optional, so this never fails just
/// because one (or all) of them is absent.
pub fn resolve_attached_config<'a>(
    registry_config: Option<&'a str>,
    run_start_config: Option<&'a str>,
    process_args_config: Option<&'a str>,
) -> Option<&'a str> {
    registry_config.or(run_start_config).or(process_args_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedLiveness(std::collections::HashSet<u32>);
    impl Liveness for FixedLiveness {
        fn is_alive(&self, pid: u32) -> bool {
            self.0.contains(&pid)
        }
    }

    fn sample_entry(run_id: &str, pid: u32, started_at: &str) -> RegistryEntry {
        RegistryEntry {
            run_id: run_id.to_string(),
            orchestration: "/tmp/doc.yml".to_string(),
            config: Some("/tmp/config.json".to_string()),
            engine: "cof".to_string(),
            pid,
            launcher: "osi 0.1.0".to_string(),
            run_dir: "/tmp/run-dir".to_string(),
            events: "/tmp/run-dir/events.jsonl".to_string(),
            live_state: "/tmp/run-dir/state.live.json".to_string(),
            started_at: started_at.to_string(),
        }
    }

    #[test]
    fn write_read_and_remove_round_trip() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        let entry = sample_entry("run-1", 4242, "2026-01-01T00:00:00Z");
        write_entry(&dir, &entry).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let file_mode = fs::metadata(registry_file_path(&dir, "run-1"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);

        let liveness = FixedLiveness([4242].into_iter().collect());
        let entries = read_entries(&dir, &liveness);
        assert_eq!(entries, vec![entry]);

        remove_entry(&dir, "run-1").unwrap();
        assert!(read_entries(&dir, &liveness).is_empty());
    }

    #[test]
    fn remove_entry_on_an_already_gone_file_is_not_an_error() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        fs::create_dir_all(&dir).unwrap();
        remove_entry(&dir, "never-written").unwrap();
    }

    #[test]
    fn read_entries_prunes_a_dead_pid() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        write_entry(&dir, &sample_entry("dead", 999, "2026-01-01T00:00:00Z")).unwrap();
        write_entry(&dir, &sample_entry("alive", 1000, "2026-01-01T00:00:01Z")).unwrap();

        let liveness = FixedLiveness([1000].into_iter().collect());
        let entries = read_entries(&dir, &liveness);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, "alive");
        // Pruned means removed from disk too, not just left out.
        assert!(!registry_file_path(&dir, "dead").exists());
        assert!(registry_file_path(&dir, "alive").exists());
    }

    #[test]
    fn read_entries_prunes_a_recycled_pid_that_is_alive_but_not_an_engine() {
        // The same pid as a live entry, but `FixedLiveness` here
        // stands in for a pid that exists yet whose command line no
        // longer names `cof`/`electricity` (a recycled pid) -- from
        // this module's own point of view that is indistinguishable
        // from "not alive" at all: [`Liveness::is_alive`] already
        // folds that check in, so a `false` answer prunes either way.
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        write_entry(&dir, &sample_entry("recycled", 555, "2026-01-01T00:00:00Z")).unwrap();
        let liveness = FixedLiveness(std::collections::HashSet::new());
        assert!(read_entries(&dir, &liveness).is_empty());
        assert!(!registry_file_path(&dir, "recycled").exists());
    }

    #[test]
    fn read_entries_skips_a_corrupt_file_without_erroring() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("broken.json"), b"{not json").unwrap();
        write_entry(&dir, &sample_entry("ok", 1, "2026-01-01T00:00:00Z")).unwrap();

        let liveness = FixedLiveness([1].into_iter().collect());
        let entries = read_entries(&dir, &liveness);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, "ok");
    }

    #[test]
    fn read_entries_orders_oldest_started_at_first() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        write_entry(&dir, &sample_entry("later", 2, "2026-01-01T00:00:05Z")).unwrap();
        write_entry(&dir, &sample_entry("earlier", 1, "2026-01-01T00:00:00Z")).unwrap();

        let liveness = FixedLiveness([1, 2].into_iter().collect());
        let entries = read_entries(&dir, &liveness);
        assert_eq!(
            entries
                .iter()
                .map(|e| e.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["earlier", "later"]
        );
    }

    #[test]
    fn resolve_runs_dir_prefers_the_override() {
        let dir = resolve_runs_dir(
            Some(Path::new("/override")),
            Some(Path::new("/run")),
            Some(Path::new("/state")),
            Some(Path::new("/home/x")),
        );
        assert_eq!(dir, Some(PathBuf::from("/override")));
    }

    #[test]
    fn resolve_runs_dir_prefers_xdg_runtime_dir_over_state_home() {
        let dir = resolve_runs_dir(
            None,
            Some(Path::new("/run/user/1000")),
            Some(Path::new("/state")),
            Some(Path::new("/home/x")),
        );
        assert_eq!(
            dir,
            Some(PathBuf::from("/run/user/1000/circuitry/osi/runs"))
        );
    }

    #[test]
    fn resolve_runs_dir_falls_back_to_state_home_then_home() {
        let dir = resolve_runs_dir(None, None, Some(Path::new("/state")), None);
        assert_eq!(dir, Some(PathBuf::from("/state/circuitry/osi/runs")));

        let dir = resolve_runs_dir(None, None, None, Some(Path::new("/home/x")));
        assert_eq!(
            dir,
            Some(PathBuf::from("/home/x/.local/state/circuitry/osi/runs"))
        );

        assert_eq!(resolve_runs_dir(None, None, None, None), None);
    }

    #[test]
    fn command_line_names_engine_matches_a_bare_name_or_a_full_path() {
        assert!(command_line_names_engine("cof run doc.yml"));
        assert!(command_line_names_engine("/usr/local/bin/cof run doc.yml"));
        assert!(command_line_names_engine("electricity config.json doc.yml"));
        assert!(!command_line_names_engine("python3 some_other_tool.py"));
        assert!(!command_line_names_engine(""));
    }

    #[test]
    fn resolve_attached_config_follows_the_fallback_order() {
        assert_eq!(
            resolve_attached_config(
                Some("registry.json"),
                Some("run-start.json"),
                Some("argv.json")
            ),
            Some("registry.json")
        );
        assert_eq!(
            resolve_attached_config(None, Some("run-start.json"), Some("argv.json")),
            Some("run-start.json")
        );
        assert_eq!(
            resolve_attached_config(None, None, Some("argv.json")),
            Some("argv.json")
        );
        assert_eq!(resolve_attached_config(None, None, None), None);
    }
}
