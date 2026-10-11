//! `cof run`'s own stderr `WARNING:`/`Warning:` lines, reproduced on
//! `electricity`'s own stderr (issue #442's "Warning lines on stderr"
//! item): every case in `golden/warning_lines.json`
//! (`scripts/generate_cli_warning_lines.py`, a real `cof run` per case)
//! is replayed against the actual `electricity` binary here, and its
//! own stderr is compared byte for byte against that recording.
//!
//! Every case uses a relative `--live-state`/`--events` path and a
//! fixed `cwd` (this test's own temp directory, exactly like the
//! generator's), so the warning text itself (which embeds that path
//! verbatim) never depends on a temporary-directory name and needs no
//! normalization.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = env::temp_dir().join(format!(
            "electricity-cli-warnings-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TempHome { path }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    orchestration: String,
    config: Option<serde_json::Value>,
    extra_args: Vec<String>,
    touch_blocker: Option<String>,
    expected_stderr: String,
    os_error_prefix: Option<String>,
}

fn cases() -> Vec<Case> {
    let text = include_str!("golden/warning_lines.json");
    serde_json::from_str(text).expect("golden/warning_lines.json must parse")
}

/// Compares *actual* against *expected* line by line, verbatim --
/// except on the one line containing *os_error_prefix* (if any), which
/// is compared only up to and including that prefix: the OS-specific
/// text after it is never byte-for-byte identical between Python's
/// `OSError.__str__` and Rust's `io::Error::Display`
/// (`generate_cli_warning_lines.py::build_case`'s own doc comment).
fn assert_stderr_matches(
    case_name: &str,
    actual: &str,
    expected: &str,
    os_error_prefix: Option<&str>,
) {
    let actual_lines: Vec<&str> = actual.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();
    assert_eq!(
        actual_lines.len(),
        expected_lines.len(),
        "case {case_name}: line count mismatch\n  got: {actual:?}\n  want: {expected:?}"
    );
    for (actual_line, expected_line) in actual_lines.iter().zip(expected_lines.iter()) {
        let lenient = os_error_prefix.is_some_and(|prefix| expected_line.contains(prefix));
        if lenient {
            let prefix = os_error_prefix.unwrap();
            let expected_head =
                &expected_line[..expected_line.find(prefix).unwrap() + prefix.len()];
            assert!(
                actual_line.starts_with(expected_head),
                "case {case_name}: line mismatch (OS-error prefix)\n  got: {actual_line:?}\n  want prefix: {expected_head:?}"
            );
        } else {
            assert_eq!(
                actual_line, expected_line,
                "case {case_name}: line mismatch\n  got: {actual_line:?}\n  want: {expected_line:?}"
            );
        }
    }
}

#[test]
fn electricity_reproduces_cof_runs_own_stderr_warning_lines() {
    for case in cases() {
        let home = TempHome::new(&case.name);
        let case_dir = home.path.join("case");
        fs::create_dir_all(&case_dir).unwrap();
        let home_dir = home.path.join("home");
        fs::create_dir_all(&home_dir).unwrap();

        let doc_path = case_dir.join("orchestration.yml");
        fs::write(&doc_path, &case.orchestration).unwrap();

        let config_value = case.config.clone().unwrap_or_else(|| serde_json::json!({}));
        let config_path = case_dir.join("config.json");
        fs::write(&config_path, config_value.to_string()).unwrap();

        if let Some(blocker) = &case.touch_blocker {
            let blocker_path = case_dir.join(blocker);
            fs::create_dir_all(blocker_path.parent().unwrap()).unwrap();
            fs::write(&blocker_path, "").unwrap();
        }

        let mut cmd = Command::new(bin());
        cmd.env_clear();
        cmd.env("HOME", &home_dir);
        cmd.env("PATH", env::var("PATH").unwrap_or_default());
        cmd.current_dir(&case_dir);
        cmd.arg("config.json").arg("orchestration.yml");
        cmd.args(&case.extra_args);

        let output = cmd.output().expect("electricity should run");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_stderr_matches(
            &case.name,
            &stderr,
            &case.expected_stderr,
            case.os_error_prefix.as_deref(),
        );
    }
}
