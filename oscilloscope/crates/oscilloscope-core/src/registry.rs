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
//! prunes an entry whose pid is positively [`PidLiveness::Gone`] or
//! [`PidLiveness::Recycled`] (its command line no longer names
//! `cof`/`electricity`), through the injectable [`Liveness`] trait --
//! no test ever checks a fixture pid against the real machine. A
//! [`PidLiveness::Unknown`] answer (the check couldn't tell either
//! way) never prunes an entry on its own. `cof` is a pip console
//! script, so a real `cof run`'s own command line (`ps -o args=`)
//! names the Python interpreter first and `cof` second;
//! [`command_line_names_engine`] recognises both shapes, and the
//! same recognition is shared with the process scan
//! ([`crate::discovery::match_scanned_process`]).

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

/// A liveness check's own answer about one pid -- four outcomes,
/// not a bool, because "couldn't tell" must never read the same as
/// a positive "it's gone": [`read_entries`] prunes an entry only on
/// [`Gone`](PidLiveness::Gone) or [`Recycled`](PidLiveness::Recycled);
/// [`Unknown`](PidLiveness::Unknown) leaves it in place, same as
/// [`Alive`](PidLiveness::Alive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidLiveness {
    /// `ps` exits 0 and the command line still names `cof` or
    /// `electricity`.
    Alive,
    /// `ps` exits non-zero with empty stdout and empty stderr --
    /// what macOS and procps both print for a pid that doesn't
    /// exist -- or `kill(pid, 0)` reports `ESRCH`.
    Gone,
    /// `ps` exits 0, but the command line no longer names an engine:
    /// the pid exists, but it's been recycled for an unrelated
    /// process.
    Recycled,
    /// Anything else -- `ps` couldn't even start, a non-zero exit
    /// came with something on stderr (not the clean "no such
    /// process" shape), and `kill(pid, 0)` didn't report `ESRCH`
    /// either. An entry is never pruned on this alone.
    Unknown,
}

/// Whether a pid named in a registry entry (or found by a process
/// scan) is still a real, running `cof`/`electricity` -- behind a
/// trait so a test injects the answer rather than ever checking a
/// fixture's pid against the real machine.
pub trait Liveness {
    fn check(&self, pid: u32) -> PidLiveness;
}

/// [`Liveness`] backed by a real `ps -ww -o args= -p <pid>` call,
/// corroborated by `kill(pid, 0)` for the shapes `ps` alone can't
/// settle -- see [`PidLiveness`] for exactly which outcome each
/// combination produces. A bare `kill(pid, 0)` on its own can be
/// fooled by a recycled pid, but `ps` always reports whatever
/// process actually holds that pid right now, recycled or not.
pub struct RealLiveness;

impl Liveness for RealLiveness {
    fn check(&self, pid: u32) -> PidLiveness {
        pid_liveness_from_ps_result(liveness_ps_command(pid).output(), kill_reports_esrch(pid))
    }
}

/// `kill(pid, 0)`'s own answer to "is this pid definitely gone" --
/// sends no signal, and only `ESRCH` is a confirmed death (anything
/// else, most commonly success or `EPERM`, means the pid is still
/// held by *some* process).
fn kill_reports_esrch(pid: u32) -> bool {
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
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

/// [`RealLiveness::check`]'s own decision, pulled out so a test can
/// hand it a synthesized `ps` result (and kill answer) for every
/// shape without touching `PATH` or any other process-wide state a
/// parallel test run shares.
fn pid_liveness_from_ps_result(
    result: std::io::Result<std::process::Output>,
    kill_reports_esrch: bool,
) -> PidLiveness {
    let Ok(output) = result else {
        return if kill_reports_esrch {
            PidLiveness::Gone
        } else {
            PidLiveness::Unknown
        };
    };
    if output.status.success() {
        let command_line = String::from_utf8_lossy(&output.stdout);
        if command_line_names_engine(&command_line) {
            return PidLiveness::Alive;
        }
        // A placeholder in place of the command line means `ps` couldn't
        // read this process's arguments, not that the pid now belongs to
        // something else: BSD `ps` prints the process name in parentheses
        // (`(python3.11)`) and procps prints it in brackets (`[python3]`).
        if is_placeholder_command_line(&command_line) {
            return PidLiveness::Unknown;
        }
        return PidLiveness::Recycled;
    }
    // Only a `ps` that exited (not one killed by a signal) with nothing to
    // say has looked the pid up and found nothing.
    if output.status.code().is_some() && output.stdout.is_empty() && output.stderr.is_empty() {
        return PidLiveness::Gone;
    }
    if kill_reports_esrch {
        PidLiveness::Gone
    } else {
        PidLiveness::Unknown
    }
}

/// Whether `ps` printed a placeholder name instead of a process's real
/// command line: `(name)` from BSD `ps`, `[name]` from procps, either
/// alone or followed by procps's ` <defunct>`.
fn is_placeholder_command_line(command_line: &str) -> bool {
    let first = command_line.split_whitespace().next().unwrap_or("");
    (first.starts_with('(') && first.ends_with(')'))
        || (first.starts_with('[') && first.ends_with(']'))
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

/// Every entry in `dir` whose pid isn't positively gone (per
/// `liveness`), oldest `started_at` first -- a corrupt, partial or
/// otherwise unparseable file is skipped, not an error, the same
/// tolerance [`crate::history::read_records`] already gives a bad
/// line. An entry is removed from disk right here, not just left out
/// of the returned list, only on a [`PidLiveness::Gone`] or
/// [`PidLiveness::Recycled`] answer; [`PidLiveness::Unknown`] keeps
/// the entry, on disk and in the returned list, exactly like
/// [`PidLiveness::Alive`] (issue #807).
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
        match liveness.check(entry.pid) {
            PidLiveness::Gone | PidLiveness::Recycled => {
                let _ = fs::remove_file(&path);
            }
            PidLiveness::Alive | PidLiveness::Unknown => entries.push(entry),
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

    /// A pid not named here answers [`PidLiveness::Gone`] -- most
    /// tests only care about one or two pids, and "anything else is
    /// gone" matches [`FixedLiveness`]'s old, bool-shaped behaviour.
    struct FixedLiveness(std::collections::HashMap<u32, PidLiveness>);
    impl FixedLiveness {
        fn alive(pids: impl IntoIterator<Item = u32>) -> Self {
            FixedLiveness(pids.into_iter().map(|p| (p, PidLiveness::Alive)).collect())
        }
    }
    impl Liveness for FixedLiveness {
        fn check(&self, pid: u32) -> PidLiveness {
            self.0.get(&pid).copied().unwrap_or(PidLiveness::Gone)
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

        let liveness = FixedLiveness::alive([4242]);
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

        let liveness = FixedLiveness::alive([1000]);
        let entries = read_entries(&dir, &liveness);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, "alive");
        // Pruned means removed from disk too, not just left out.
        assert!(!registry_file_path(&dir, "dead").exists());
        assert!(registry_file_path(&dir, "alive").exists());
    }

    #[test]
    fn read_entries_prunes_a_recycled_pid_that_is_alive_but_not_an_engine() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        write_entry(&dir, &sample_entry("recycled", 555, "2026-01-01T00:00:00Z")).unwrap();
        let liveness = FixedLiveness([(555, PidLiveness::Recycled)].into_iter().collect());
        assert!(read_entries(&dir, &liveness).is_empty());
        assert!(!registry_file_path(&dir, "recycled").exists());
    }

    #[test]
    fn read_entries_keeps_an_entry_on_unknown_and_removes_it_on_gone() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        write_entry(&dir, &sample_entry("unclear", 1, "2026-01-01T00:00:00Z")).unwrap();
        write_entry(&dir, &sample_entry("gone", 2, "2026-01-01T00:00:01Z")).unwrap();

        let liveness = FixedLiveness(
            [(1, PidLiveness::Unknown), (2, PidLiveness::Gone)]
                .into_iter()
                .collect(),
        );
        let entries = read_entries(&dir, &liveness);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, "unclear");
        assert!(registry_file_path(&dir, "unclear").exists());
        assert!(!registry_file_path(&dir, "gone").exists());
    }

    #[test]
    fn read_entries_skips_a_corrupt_file_without_erroring() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("runs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("broken.json"), b"{not json").unwrap();
        write_entry(&dir, &sample_entry("ok", 1, "2026-01-01T00:00:00Z")).unwrap();

        let liveness = FixedLiveness::alive([1]);
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

        let liveness = FixedLiveness::alive([1, 2]);
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

    fn fake_ps_output(success: bool, stdout: &str, stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn pid_liveness_from_ps_result_is_unknown_when_ps_prints_a_placeholder_name() {
        for placeholder in ["(python3.11)\n", "(cof)", "[python3]", "[cof] <defunct>"] {
            let output = fake_ps_output(true, placeholder, "");
            assert_eq!(
                pid_liveness_from_ps_result(Ok(output), false),
                PidLiveness::Unknown,
                "{placeholder:?}"
            );
        }
    }

    #[test]
    fn pid_liveness_from_ps_result_is_unknown_when_ps_is_killed_by_a_signal() {
        use std::os::unix::process::ExitStatusExt;
        let killed = || std::process::Output {
            status: std::process::ExitStatus::from_raw(libc::SIGKILL),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(
            pid_liveness_from_ps_result(Ok(killed()), false),
            PidLiveness::Unknown
        );
        assert_eq!(
            pid_liveness_from_ps_result(Ok(killed()), true),
            PidLiveness::Gone
        );
    }

    #[test]
    fn pid_liveness_from_ps_result_is_alive_when_ps_succeeds_and_names_an_engine() {
        let output = fake_ps_output(true, "cof run doc.yml", "");
        assert_eq!(
            pid_liveness_from_ps_result(Ok(output), false),
            PidLiveness::Alive
        );
    }

    #[test]
    fn pid_liveness_from_ps_result_is_recycled_when_ps_succeeds_but_names_no_engine() {
        let output = fake_ps_output(true, "some_other_tool --flag", "");
        assert_eq!(
            pid_liveness_from_ps_result(Ok(output), false),
            PidLiveness::Recycled
        );
        // Even a kill(pid, 0) that (wrongly, for a live pid) reports
        // ESRCH can't override a `ps` that actually succeeded.
        let output = fake_ps_output(true, "some_other_tool --flag", "");
        assert_eq!(
            pid_liveness_from_ps_result(Ok(output), true),
            PidLiveness::Recycled
        );
    }

    #[test]
    fn pid_liveness_from_ps_result_is_gone_when_ps_fails_with_empty_output() {
        // The "no such process" shape macOS and procps both print:
        // non-zero exit, nothing on stdout or stderr.
        let output = fake_ps_output(false, "", "");
        assert_eq!(
            pid_liveness_from_ps_result(Ok(output), false),
            PidLiveness::Gone
        );
    }

    #[test]
    fn pid_liveness_from_ps_result_is_gone_when_kill_confirms_esrch_despite_an_unclear_ps() {
        // `ps` failed, but not in the clean empty/empty shape --
        // `kill(pid, 0)` reporting ESRCH still settles it.
        let output = fake_ps_output(false, "", "ps: some platform-specific error\n");
        assert_eq!(
            pid_liveness_from_ps_result(Ok(output), true),
            PidLiveness::Gone
        );
        // Same for `ps` not starting at all.
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        assert_eq!(
            pid_liveness_from_ps_result(Err(err), true),
            PidLiveness::Gone
        );
    }

    #[test]
    fn pid_liveness_from_ps_result_is_unknown_when_ps_could_not_even_run() {
        // `Command::output` itself failing (no `ps` on `PATH` at
        // all) must never read as "the pid is dead": that would
        // prune a live entry on nothing more than a missing `ps`.
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        assert_eq!(
            pid_liveness_from_ps_result(Err(err), false),
            PidLiveness::Unknown
        );
    }

    #[test]
    fn pid_liveness_from_ps_result_is_unknown_when_ps_fails_with_stderr_and_kill_does_not_confirm()
    {
        let output = fake_ps_output(false, "", "ps: some platform-specific error\n");
        assert_eq!(
            pid_liveness_from_ps_result(Ok(output), false),
            PidLiveness::Unknown
        );
    }

    #[test]
    fn real_ps_reports_a_process_named_cof_as_alive() {
        // This test binary's own pid isn't named `cof`/`electricity`,
        // so this spawns `/bin/sleep` with its argv[0] overridden to
        // `cof` -- a real `ps` reports exactly that overridden argv
        // (not the binary's real path), on both macOS and Linux.
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg0("cof")
            .arg("5")
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(RealLiveness.check(pid), PidLiveness::Alive);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn real_ps_reports_a_reaped_pid_as_gone() {
        // A pid this process spawned and already reaped: guaranteed
        // not to be held by this process any more, so a real `ps`
        // (and `kill(pid, 0)`) see exactly the "no such process"
        // case this issue is about.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(RealLiveness.check(pid), PidLiveness::Gone);
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
