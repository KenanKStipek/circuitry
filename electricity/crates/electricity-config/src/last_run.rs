//! `cli/last_run.py`, both directions: where the `--last`/`--resume last`
//! stash lives, how an unreadable or malformed one is reported (never by
//! raising -- a bad stash is a [`LastRun`] carrying `error`, same as the
//! reference's own total `read_last_run`), and how a run writes one
//! (`cli/app.py::_save_last_run`, best-effort, silently swallowing a write
//! failure the same way the reference does).
//!
//! electricity writes and reads the exact same file, with the exact same
//! keys, key order and JSON formatting (issue #760) -- a stash either
//! engine wrote replays on the other, and in the TUI's own Runs view.

use crate::config::global_config_path;
use electricity_redaction::redact_env_pairs;
use electricity_value::{Dict, Value};
use std::path::{Path, PathBuf};

/// `cli/last_run.py::LAST_RUN_PATH`: `<home>/.config/circuitry/last-run.json`,
/// beside the global config and the trust store.
pub fn last_run_path(home: &Path) -> PathBuf {
    global_config_path(home)
        .parent()
        .expect("global_config_path always has a parent")
        .join("last-run.json")
}

/// `cli/last_run.py::REDACTED_REFUSAL`'s own trigger -- re-exported here
/// so a caller that only needs the redaction check doesn't have to pull
/// in `electricity_redaction` itself just to call [`redact_env_pairs`]
/// again.
pub use electricity_redaction::REDACTED;

/// `cli/last_run.py::LastRun` -- the stashed arguments of the most recent
/// `electricity run`/`cof run`, successful or not. `args` is empty and
/// `error` is non-empty when the file exists but couldn't be read or
/// parsed as a JSON object; never both at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    pub path: PathBuf,
    pub args: Dict,
    pub error: String,
}

impl LastRun {
    /// `LastRun.ok`.
    pub fn ok(&self) -> bool {
        self.error.is_empty()
    }

    fn str_field(&self, key: &str) -> Option<String> {
        match self.args.get(&Value::Str(key.to_string())) {
            Some(Value::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    }

    /// `LastRun.orchestration` -- `""` when the stash names none (a
    /// malformed or ancient stash).
    pub fn orchestration(&self) -> String {
        self.str_field("orchestration").unwrap_or_default()
    }

    /// `LastRun.out_path`.
    pub fn out_path(&self) -> Option<PathBuf> {
        self.str_field("out").map(PathBuf::from)
    }

    /// `LastRun.config_path`.
    pub fn config_path(&self) -> Option<PathBuf> {
        self.str_field("config").map(PathBuf::from)
    }

    /// `LastRun.state_path` -- the plain `--state <path>` flag's own raw
    /// value (seed-the-whole-run, no skip), distinct from [`Self::out_path`]
    /// (what `--resume last` resumes from).
    pub fn state_path(&self) -> Option<PathBuf> {
        self.str_field("state").map(PathBuf::from)
    }

    /// `LastRun.live_state_path`.
    pub fn live_state_path(&self) -> Option<PathBuf> {
        self.str_field("live_state").map(PathBuf::from)
    }

    /// `events` -- not one of `LastRun`'s own reference properties (the
    /// reference's own TUI replay never needs it), but `electricity
    /// run --last` replays it the same way it replays `live_state`.
    pub fn events_path(&self) -> Option<PathBuf> {
        self.str_field("events").map(PathBuf::from)
    }

    /// `profile` -- a bare name for the reference, an explicit path for
    /// electricity's own `--profile <path>` (DESIGN.md §6.11); the stash
    /// carries whichever shape the run that wrote it used.
    pub fn profile(&self) -> Option<String> {
        self.str_field("profile")
    }

    pub fn adapter(&self) -> Option<String> {
        self.str_field("adapter")
    }

    pub fn label(&self) -> Option<String> {
        self.str_field("label")
    }

    pub fn allow_capabilities(&self) -> Option<String> {
        self.str_field("allow_capabilities")
    }

    /// `LastRun.env_pairs` -- every `KEY=VALUE` string in the stashed
    /// `env_vars` list; a non-string entry is dropped, matching the
    /// reference's own tolerant read.
    pub fn env_pairs(&self) -> Vec<String> {
        match self.args.get(&Value::Str("env_vars".to_string())) {
            Some(Value::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `LastRun.has_redacted_secrets` -- true when replaying `env_pairs`
    /// would send the literal redaction marker as a real value.
    pub fn has_redacted_secrets(&self) -> bool {
        let marker = format!("={REDACTED}");
        self.env_pairs().iter().any(|pair| pair.ends_with(&marker))
    }

    fn bool_field(&self, key: &str) -> bool {
        matches!(
            self.args.get(&Value::Str(key.to_string())),
            Some(Value::Bool(true))
        )
    }

    /// `Some(b)` only when the stash carries a real boolean for *key* --
    /// `cli/last_run.py::LastRun.scoring`/`routing`/`decompose`/`split`'s
    /// own `value if isinstance(value, bool) else None`. `None` for a
    /// missing key or an explicit JSON `null` either way, matching
    /// `cof run`'s own three-state `--scoring`/`--no-scoring`/unset flags.
    fn optional_bool_field(&self, key: &str) -> Option<bool> {
        match self.args.get(&Value::Str(key.to_string())) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn pretty(&self) -> bool {
        self.bool_field("pretty")
    }

    pub fn quiet(&self) -> bool {
        self.bool_field("quiet")
    }

    /// `LastRun.trust_document`.
    pub fn trust_document(&self) -> bool {
        self.bool_field("trust_document")
    }

    /// `LastRun.remote_library_source`.
    pub fn remote_library_source(&self) -> bool {
        self.bool_field("remote_library_source")
    }

    /// `LastRun.dry_run`.
    pub fn dry_run(&self) -> bool {
        self.bool_field("dry_run")
    }

    /// `cli/app.py::run_cmd`'s own `skip_preflight` stash field --
    /// distinct from [`LastRun::has_redacted_secrets`], electricity's
    /// own CLI has no `--skip-preflight` flag at all (settled ruling 1:
    /// preflight always runs).
    pub fn skip_preflight(&self) -> bool {
        self.bool_field("skip_preflight")
    }

    /// `--service-profile <name>` (`cof run-library` only) -- electricity
    /// has no equivalent of its own.
    pub fn service_profile(&self) -> Option<String> {
        self.str_field("service_profile")
    }

    /// `LastRun.model`.
    pub fn model(&self) -> Option<String> {
        self.str_field("model")
    }

    /// `LastRun.profile_from_state`.
    pub fn profile_from_state(&self) -> Option<String> {
        self.str_field("profile_from_state")
    }

    /// `LastRun.scoring`.
    pub fn scoring(&self) -> Option<bool> {
        self.optional_bool_field("scoring")
    }

    /// `LastRun.routing`.
    pub fn routing(&self) -> Option<bool> {
        self.optional_bool_field("routing")
    }

    /// `LastRun.decompose`.
    pub fn decompose(&self) -> Option<bool> {
        self.optional_bool_field("decompose")
    }

    /// `LastRun.split`.
    pub fn split(&self) -> Option<bool> {
        self.optional_bool_field("split")
    }

    /// `--decompose-out <path>` -- electricity has no `--decompose`/
    /// `--decompose-out` flags of its own.
    pub fn decompose_out(&self) -> Option<String> {
        self.str_field("decompose_out")
    }

    /// The first setting in the stash electricity's own `--last` has no
    /// way to replay faithfully, worded for a refusal message -- `None`
    /// when every one of them is at the default a plain `electricity run`
    /// (or `cof run`, naming a trusted local file with no extra flags)
    /// would have stashed, the only shape `--last` replays in full.
    /// Checked in a fixed order so the same stash always names the same
    /// setting.
    pub fn unreplayable_setting(&self) -> Option<&'static str> {
        if self.remote_library_source() {
            return Some("a remote library source");
        }
        if !self.trust_document() {
            return Some("an orchestration resolved from a library name, not a trusted local file");
        }
        if self.service_profile().is_some() {
            return Some("--service-profile");
        }
        if self.dry_run() {
            return Some("--dry-run");
        }
        if self.skip_preflight() {
            return Some("--skip-preflight");
        }
        if self.model().is_some() {
            return Some("--model");
        }
        if self.profile_from_state().is_some() {
            return Some("--profile-from-state");
        }
        if self.scoring().is_some() {
            return Some("--scoring/--no-scoring");
        }
        if self.routing().is_some() {
            return Some("--routing/--no-routing");
        }
        if self.decompose().is_some() {
            return Some("--decompose/--no-decompose");
        }
        if self.split().is_some() {
            return Some("--split/--no-split");
        }
        if self.decompose_out().is_some() {
            return Some("--decompose-out");
        }
        None
    }
}

/// `cli/last_run.py::read_last_run` -- total: a missing stash is `None`;
/// an unreadable or malformed one is a [`LastRun`] carrying `error`
/// rather than a propagated error, because a corrupt file must render as
/// an error state in a view (or a refusal message), never panic or bubble
/// an `io`/parse error through a caller that has no use for one.
pub fn read_last_run(path: &Path) -> Option<LastRun> {
    let raw = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            return Some(LastRun {
                path: path.to_path_buf(),
                args: Dict::new(),
                error: format!("Could not read {}: {err}", path.display()),
            });
        }
    };
    let parsed = match electricity_json::loads(&raw) {
        Ok(value) => value,
        Err(err) => {
            // `cli/last_run.py::read_last_run`'s own `exc.msg` -- the bare
            // `JSONDecodeError` message, never `str(exc)`'s own trailing
            // `: line L column C (char P)` (`ReadError::Syntax`'s own
            // `Display` includes that suffix; this strips it back off to
            // match).
            let message = match &err {
                electricity_json::ReadError::Syntax { message, .. } => message.clone(),
                other => other.to_string(),
            };
            return Some(LastRun {
                path: path.to_path_buf(),
                args: Dict::new(),
                error: format!("{} is not valid JSON \u{2014} {message}.", path.display()),
            });
        }
    };
    let mut parsed = parsed;
    match &mut parsed {
        // `Value`'s own iterative `Drop` impl forbids moving `dict` out of
        // `parsed` directly (`electricity-redaction`'s own identical
        // comment, same root cause) -- `mem::take` swaps it out instead.
        Value::Dict(dict) => Some(LastRun {
            path: path.to_path_buf(),
            args: std::mem::take(dict),
            error: String::new(),
        }),
        _ => Some(LastRun {
            path: path.to_path_buf(),
            args: Dict::new(),
            error: format!("{} does not hold a JSON object.", path.display()),
        }),
    }
}

/// `cli/app.py::_save_last_run` -- best-effort: a directory that can't be
/// created, or a file that can't be written, is silently swallowed, never
/// failing (or even warning on) the run it was stashing. `args`' own key
/// order is preserved as given (`Dict`'s own insertion order), so a
/// caller builds it in the exact order `cli/app.py`'s own dict literal
/// does to match the reference's own file byte for byte.
pub fn write_last_run(path: &Path, args: Dict) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(text) =
        electricity_json::dumps(&Value::Dict(args), electricity_json::WriteMode::COMPACT)
    else {
        return;
    };
    let _ = std::fs::write(path, format!("{text}\n"));
}

/// Redacts every credential-shaped `KEY=VALUE` entry in *pairs* the same
/// way `cli/app.py::_save_last_run` does before writing `env_vars` to
/// disk (`cli/redaction.py::redact_env_pairs`) -- re-exported under this
/// module so a caller building a stash doesn't need its own
/// `electricity_redaction` import just for this one call.
pub fn redacted_env_pairs(pairs: &[String]) -> Vec<String> {
    redact_env_pairs(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tmp: &std::path::Path) -> PathBuf {
        tmp.to_path_buf()
    }

    #[test]
    fn last_run_path_sits_beside_the_global_config() {
        let dir = std::env::temp_dir().join("electricity-last-run-path-test");
        let path = last_run_path(&home(&dir));
        assert_eq!(
            path,
            dir.join(".config").join("circuitry").join("last-run.json")
        );
    }

    #[test]
    fn read_last_run_is_none_when_the_file_is_missing() {
        let dir = tempfile_dir("missing");
        assert!(read_last_run(&dir.join("last-run.json")).is_none());
    }

    #[test]
    fn read_last_run_reports_invalid_json_as_an_error_not_a_panic() {
        let dir = tempfile_dir("bad-json");
        let path = dir.join("last-run.json");
        std::fs::write(&path, "not json").unwrap();
        let stash = read_last_run(&path).expect("a present, unreadable file is Some");
        assert!(!stash.ok());
        assert!(stash.error.contains("not valid JSON"));
    }

    #[test]
    fn read_last_run_reports_a_non_object_as_an_error() {
        let dir = tempfile_dir("non-object");
        let path = dir.join("last-run.json");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        let stash = read_last_run(&path).expect("present");
        assert!(!stash.ok());
        assert!(stash.error.contains("does not hold a JSON object"));
    }

    #[test]
    fn write_then_read_round_trips_env_vars_and_orchestration() {
        let dir = tempfile_dir("round-trip");
        let path = dir.join("nested").join("last-run.json");
        let mut args = Dict::new();
        args.insert(
            Value::Str("orchestration".to_string()),
            Value::Str("/tmp/doc.yml".to_string()),
        );
        args.insert(
            Value::Str("env_vars".to_string()),
            Value::List(vec![Value::Str("name=World".to_string())]),
        );
        write_last_run(&path, args);
        let stash = read_last_run(&path).expect("just written");
        assert!(stash.ok());
        assert_eq!(stash.orchestration(), "/tmp/doc.yml");
        assert_eq!(stash.env_pairs(), vec!["name=World".to_string()]);
    }

    #[test]
    fn has_redacted_secrets_detects_the_sentinel_suffix() {
        let mut args = Dict::new();
        args.insert(
            Value::Str("env_vars".to_string()),
            Value::List(vec![Value::Str(format!("OPENAI_API_KEY={REDACTED}"))]),
        );
        let stash = LastRun {
            path: PathBuf::from("x"),
            args,
            error: String::new(),
        };
        assert!(stash.has_redacted_secrets());
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("electricity-last-run-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn minimal_args() -> Dict {
        let mut args = Dict::new();
        args.insert(
            Value::Str("orchestration".to_string()),
            Value::Str("orchestration.yml".to_string()),
        );
        args.insert(Value::Str("trust_document".to_string()), Value::Bool(true));
        args.insert(
            Value::Str("remote_library_source".to_string()),
            Value::Bool(false),
        );
        args
    }

    #[test]
    fn unreplayable_setting_is_none_for_a_plain_run() {
        let stash = LastRun {
            path: PathBuf::from("x"),
            args: minimal_args(),
            error: String::new(),
        };
        assert_eq!(stash.unreplayable_setting(), None);
    }

    #[test]
    fn unreplayable_setting_names_a_remote_library_source() {
        let mut args = minimal_args();
        args.insert(
            Value::Str("remote_library_source".to_string()),
            Value::Bool(true),
        );
        let stash = LastRun {
            path: PathBuf::from("x"),
            args,
            error: String::new(),
        };
        assert_eq!(
            stash.unreplayable_setting(),
            Some("a remote library source")
        );
    }

    #[test]
    fn unreplayable_setting_names_an_untrusted_library_name() {
        let mut args = minimal_args();
        args.insert(Value::Str("trust_document".to_string()), Value::Bool(false));
        let stash = LastRun {
            path: PathBuf::from("x"),
            args,
            error: String::new(),
        };
        assert!(
            stash
                .unreplayable_setting()
                .unwrap()
                .contains("library name")
        );
    }

    #[test]
    fn unreplayable_setting_names_dry_run() {
        let mut args = minimal_args();
        args.insert(Value::Str("dry_run".to_string()), Value::Bool(true));
        let stash = LastRun {
            path: PathBuf::from("x"),
            args,
            error: String::new(),
        };
        assert_eq!(stash.unreplayable_setting(), Some("--dry-run"));
    }

    #[test]
    fn unreplayable_setting_names_scoring_when_set_to_either_bool() {
        let mut args = minimal_args();
        args.insert(Value::Str("scoring".to_string()), Value::Bool(false));
        let stash = LastRun {
            path: PathBuf::from("x"),
            args,
            error: String::new(),
        };
        assert_eq!(stash.unreplayable_setting(), Some("--scoring/--no-scoring"));
    }
}
