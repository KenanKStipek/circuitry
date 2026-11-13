//! osi's run registry: one `0600` JSON file per run this `osi`
//! launched or watched, in a `0700` directory under
//! `$XDG_RUNTIME_DIR/circuitry/osi/runs/`, else
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
//! longer names `cof`/`electricity`, through the injectable
//! [`Liveness`] trait -- no test ever checks a fixture pid against
//! the real machine. `cof` is a pip console script, so a real `cof
//! run`'s own command line (`ps -o args=`) names the Python
//! interpreter first and `cof` second; [`command_line_names_engine`]
//! recognises both shapes, and the same recognition is shared with
//! the process scan ([`crate::discovery::match_scanned_process`]).

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
/// fixture's pid against the real machine.
pub trait Liveness {
    fn is_alive(&self, pid: u32) -> bool;
}

/// [`Liveness`] backed by a real `ps -ww -o args= -p <pid>` call: alive
/// means the pid exists *and* its command line still names `cof` or
/// `electricity` -- a bare `kill(pid, 0)` can be fooled by a recycled
/// pid, but `ps` always reports whatever process actually holds that
/// pid right now, recycled or not. When `ps` itself can't be run at
/// all, this can't tell either way, so it answers `true` -- an
/// entry is never pruned on "couldn't check" alone.
pub struct RealLiveness;

impl Liveness for RealLiveness {
    fn is_alive(&self, pid: u32) -> bool {
        is_alive_from_ps_result(liveness_ps_command(pid).output())
    }
}

/// The liveness check's own `ps` call. `-ww` keeps the whole command
/// line: without it, Linux's `ps` cuts the `args` column at
/// `$COLUMNS` when stdout isn't a terminal, which can drop the
/// `cof`/`electricity` word from a long interpreter path and make a
/// live run read as gone.
pub fn liveness_ps_command(pid: u32) -> std::process::Command {
    let mut command = std::process::Command::new("ps");
    command.args(["-ww", "-o", "args=", "-p", &pid.to_string()]);
    command
}

/// [`RealLiveness::is_alive`]'s own decision, pulled out so a test
/// can hand it a synthesized "couldn't even run `ps`" error without
/// touching `PATH` or any other process-wide state a parallel test
/// run shares.
fn is_alive_from_ps_result(result: std::io::Result<std::process::Output>) -> bool {
    let Ok(output) = result else {
        return true;
    };
    if !output.status.success() {
        return false;
    }
    command_line_names_engine(&String::from_utf8_lossy(&output.stdout))
}

/// The basename of one command-line token.
fn token_basename(token: &str) -> &str {
    Path::new(token)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(token)
}

/// `cof`/`electricity`'s own engine name for a token that already
/// names one of them, directly.
fn engine_name_for_token(token: &str) -> Option<&'static str> {
    match token_basename(token) {
        "cof" => Some("cof"),
        "electricity" => Some("electricity"),
        _ => None,
    }
}

/// The engine a tokenized command line's own executable word names,
/// and the index of the first token after it. `cof` is invoked as a
/// pip console script, so a `ps`/process-scan report of a real `cof
/// run` names the Python interpreter first and `cof` second -- this
/// recognises both shapes, so [`RealLiveness`] and
/// [`crate::discovery::match_scanned_process`] treat them alike.
pub fn engine_invocation(tokens: &[&str]) -> Option<(&'static str, usize)> {
    let first = *tokens.first()?;
    if let Some(name) = engine_name_for_token(first) {
        return Some((name, 1));
    }
    if token_basename(first)
        .to_ascii_lowercase()
        .starts_with("python")
    {
        let second = *tokens.get(1)?;
        if let Some(name) = engine_name_for_token(second) {
            return Some((name, 2));
        }
    }
    None
}

/// Whether a `ps`-reported command line names `cof` or `electricity`,
/// directly or through a Python interpreter -- pulled out of
/// [`RealLiveness`] so a test exercises every case (a bare name, a
/// full path, an interpreter in front, an unrelated command) without
/// a real pid at all.
pub fn command_line_names_engine(args_line: &str) -> bool {
    let tokens: Vec<&str> = args_line.split_whitespace().collect();
    engine_invocation(&tokens).is_some()
}

/// `OSI_RUNS_DIR`, else `$XDG_RUNTIME_DIR/circuitry/osi/runs`, else
/// `$XDG_STATE_HOME/circuitry/osi/runs`, else
/// `~/.local/state/circuitry/osi/runs` --
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
/// same rule [`crate::history::ensure_history_dir`] already follows.
/// Never warns about an existing, too-open directory itself (see
/// [`insecure_runs_dir_mode`]): this runs from inside
/// [`write_entry`], which a live TUI redraw loop can reach, and a
/// stderr write there would land in the middle of the alternate
/// screen.
pub fn ensure_runs_dir(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(dir)
}

/// `dir`'s own mode, when it already exists and is group- or
/// world-accessible -- `None` when it doesn't exist yet (nothing to
/// warn about: [`ensure_runs_dir`] is about to create it at `0700`)
/// or its metadata can't be read. Checked once, by the binary crate,
/// before it ever touches the screen (osi's own startup, not from
/// inside a live redraw loop), so a warning about it can never land
/// in the middle of the TUI's own alternate-screen output.
pub fn insecure_runs_dir_mode(dir: &Path) -> Option<u32> {
    let mode = fs::metadata(dir).ok()?.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
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
/// entry whose pid is *not* alive is removed from disk right here,
/// not just left out of the returned list.
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

/// The config shown for an attached run: the registry entry's own
/// `config`, else `run_start`'s (the engine half, issue #697), else
/// whatever the process's own arguments gave, else `None` -- every
/// source is optional, so this never fails just because one (or
/// all) of them is absent.
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

    /// `-ww`: Linux's `ps` otherwise cuts command lines at `$COLUMNS`
    /// when piped, which can drop the engine's name from a long path.
    #[test]
    fn the_liveness_ps_call_asks_for_whole_command_lines() {
        let command = liveness_ps_command(4242);
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-ww", "-o", "args=", "-p", "4242"]);
    }

    /// The real `ps`, with `COLUMNS` set narrower than this test's own
    /// command line, still reports the whole of it.
    #[test]
    fn the_liveness_ps_call_ignores_a_narrow_columns() {
        let mut command = liveness_ps_command(std::process::id());
        command.env("COLUMNS", "20");
        let output = command.output().expect("ps runs");
        let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let own = std::env::args().collect::<Vec<_>>().join(" ");
        assert!(
            line.len() > 20 && line.starts_with(own.split_whitespace().next().unwrap_or("")),
            "expected the whole command line, got {line:?}"
        );
    }

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
    fn command_line_names_engine_matches_cof_run_through_its_own_interpreter() {
        // `cof` is a pip console script: the kernel reports the
        // Python interpreter first and `cof` second.
        assert!(command_line_names_engine(
            "/x/.venv/bin/python /x/.venv/bin/cof run doc.yml --live-state /x/state.live.json"
        ));
        assert!(command_line_names_engine(
            "/usr/bin/python3.11 /usr/local/bin/cof run doc.yml"
        ));
        assert!(!command_line_names_engine(
            "/usr/bin/python3.11 some_other_tool.py"
        ));
        assert!(!command_line_names_engine("/usr/bin/python3.11"));
    }

    #[test]
    fn is_alive_from_ps_result_treats_a_ps_that_could_not_even_run_as_alive() {
        // `Command::output` itself failing (no `ps` on `PATH` at
        // all) must never read as "the pid is dead": that would
        // prune a live entry on nothing more than a missing `ps`.
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        assert!(is_alive_from_ps_result(Err(err)));
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
