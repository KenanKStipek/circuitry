//! Preview skeleton of the electricity library crate.
//!
//! This release ships no VM, tool, or adapter implementation (see
//! `../../DESIGN.md`). [`run_orchestration`] runs
//! `electricity_compiler::check_for_run` first (issue #408's CLI
//! section) and only ever reports one of two outcomes --
//! [`RunOutcome::CheckFailed`] on a check failure, or
//! [`RunOutcome::PreviewRefusal`] once a document actually checks out,
//! since there is still no VM to run it with. [`dump_ir`] is a
//! separate, unstable debugging aid that runs the same check and
//! prints the result as JSON.

use std::fmt;
use std::path::Path;

/// The crate's version, taken from the workspace's `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `electricity <version> (preview)`, the string printed by `electricity --version`.
pub fn version_string() -> String {
    format!("electricity {VERSION} (preview)")
}

/// Returned by every attempt to run an orchestration in this preview release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewUnsupported;

impl fmt::Display for PreviewUnsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "electricity {VERSION} is a preview and cannot run orchestrations yet; use `cof run` instead"
        )
    }
}

impl std::error::Error for PreviewUnsupported {}

/// Parses the CLI's `-e key=value` entries, in order, into
/// [`electricity_compiler::CheckOptions`]'s own `inputs` shape --
/// `cli/app.py::_parse_env_vars`'s own `"=" not in entry` check and its
/// exact `BadParameter` text (issue #429: Circuitry's own message, not
/// third-party, so matched word for word), and its own `result[key] =
/// ...` dict-assignment semantics for a repeated key: an `IndexMap`'s
/// `insert` keeps a repeated key at its *first* occurrence's position
/// while taking the *new* value, exactly like a Python `dict`'s own
/// `__setitem__` does -- so `-e name=A -e name=B` behaves like cof's
/// own `-e name=A -e name=B` (`B` wins, in `name`'s original position).
pub fn parse_inputs(entries: &[String]) -> Result<indexmap::IndexMap<String, String>, String> {
    let mut result = indexmap::IndexMap::new();
    for entry in entries {
        match entry.split_once('=') {
            Some((key, value)) => {
                result.insert(key.to_string(), value.to_string());
            }
            None => {
                return Err(format!(
                    "Invalid -e format: {} (expected KEY=VALUE)",
                    python_repr_str(entry)
                ));
            }
        }
    }
    Ok(result)
}

/// Python `repr(s)` of a plain Rust `&str` -- [`parse_inputs`]'s own
/// malformed-entry message quotes the offending text the same way
/// Python's `{entry!r}` f-string interpolation does.
fn python_repr_str(s: &str) -> String {
    electricity_value::Value::Str(s.to_string()).py_repr()
}

/// The [`electricity_compiler::CheckOptions`] an `electricity` run of
/// *orchestration_path* against *config_path* checks against: trusting
/// the document and skipping preflight (issue #408's CLI section),
/// *config_path*'s own `runtime:` block (if the file exists and parses)
/// merged under the document's own, key by key
/// ([`config_runtime_block`]), and *inputs* (the CLI's own `-e
/// key=value` pairs, [`parse_inputs`]'s own output -- issue #429)
/// passed straight through as `CheckOptions.inputs`. The one place
/// [`run_orchestration`] and [`dump_ir`] both build their options, so
/// the two can never drift apart -- and what another tool in this
/// workspace (`oscilloscope`, which links `electricity_compiler`
/// directly) should call to compile a document with exactly the
/// options a real `electricity` run would use for it, rather than
/// reimplementing this merge itself.
pub fn check_options(
    config_path: &Path,
    inputs: &indexmap::IndexMap<String, String>,
) -> electricity_compiler::CheckOptions {
    electricity_compiler::CheckOptions {
        skip_preflight: true,
        trust_document: true,
        config_runtime: config_runtime_block(config_path),
        inputs: inputs.clone(),
    }
}

/// Runs *orchestration_path* the way `electricity <config.json> <doc> ...`
/// does (issue #408's CLI section): [`electricity_compiler::check_for_run`]
/// first, against [`check_options`]'s own options -- then, on success,
/// still this preview's one refusal, since there is no VM yet.
///
/// A [`RunOutcome::CheckFailed`] carries [`electricity_compiler::check_for_run`]'s
/// own error text verbatim -- the exact text the CLI writes to stderr on a
/// check failure (issue #408's CLI section: "On failure: exactly the error
/// text on stderr, exit 1"). [`RunOutcome::PreviewRefusal`] is the
/// unconditional "On success: keep the current preview refusal" branch.
pub fn run_orchestration(
    config_path: &Path,
    orchestration_path: &Path,
    inputs: &indexmap::IndexMap<String, String>,
) -> RunOutcome {
    let options = check_options(config_path, inputs);
    match electricity_compiler::check_for_run(orchestration_path, &options) {
        Ok(_) => RunOutcome::PreviewRefusal(PreviewUnsupported),
        Err(err) => RunOutcome::CheckFailed(err.to_string()),
    }
}

/// Why [`run_orchestration`] didn't run the orchestration -- either
/// outcome is a CLI failure (exit 1) in this preview release; the two
/// variants exist only so the CLI can tell which text to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// [`electricity_compiler::check_for_run`]'s own error text.
    CheckFailed(String),
    /// The check passed; this preview still has no VM to run it with.
    PreviewRefusal(PreviewUnsupported),
}

impl fmt::Display for RunOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunOutcome::CheckFailed(message) => write!(f, "{message}"),
            RunOutcome::PreviewRefusal(preview) => write!(f, "{preview}"),
        }
    }
}

/// *config_path*'s own `runtime:` block, or `None` when the file doesn't
/// exist, isn't valid UTF-8 JSON, or has no such key -- lenient on
/// purpose (issue #408's lane B section only asks for "what the
/// pipeline needs from the config file, i.e. its runtime: block", not
/// full config-file validation, which stays `cof`'s own job).
fn config_runtime_block(config_path: &Path) -> Option<electricity_value::Value> {
    let bytes = std::fs::read(config_path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let value = electricity_json::loads(&text).ok()?;
    value
        .as_dict()?
        .get(&electricity_value::Value::Str("runtime".to_string()))
        .cloned()
}

/// `{"ir_version": "unstable", "program": ...}`, 2-space indented, for
/// `electricity --dump-ir` (issue #408's CLI section). Not a contract:
/// nothing in this workspace reads this shape back, and it may change in
/// any release -- see `electricity_bytecode`'s crate docs.
///
/// Runs `electricity_compiler::check_for_run` first, with *config_path*'s
/// own `runtime:` block merged under the document's own the same way
/// [`run_orchestration`] does -- so a `--dump-ir` of a document that
/// relies on a config-defined `runtime.concurrency_groups`/
/// `max_concurrency` checks out the same way a `cof run`/plain run of it
/// would, rather than failing without the config file's own settings.
/// Its `Err` becomes this function's `Err`, with the exact text
/// `--dump-ir` writes to stderr on failure (the same text a plain run of
/// the same document would report).
///
/// *inputs* ([`parse_inputs`]'s own output, issue #429) is passed
/// through as `CheckOptions.inputs`, same as [`run_orchestration`] --
/// so a document with a required input needs `-e` on `--dump-ir` too,
/// exactly as it does on a plain run.
pub fn dump_ir(
    config_path: &Path,
    orchestration_path: &Path,
    inputs: &indexmap::IndexMap<String, String>,
) -> Result<String, String> {
    let options = check_options(config_path, inputs);
    let program = electricity_compiler::check_for_run(orchestration_path, &options)
        .map_err(|err| err.to_string())?;
    // `serde_json::json!` would `.unwrap()` internally on a `Program`
    // that fails to serialize (e.g. a non-UTF-8 `PathBuf` in
    // `DocumentInfo`) -- reporting that as an `Err` on stderr instead
    // of panicking matters here specifically, since a `--dump-ir`
    // failure is this crate's one promise never to crash.
    let program_value = serde_json::to_value(&program).map_err(|err| err.to_string())?;
    let mut wrapper = serde_json::Map::with_capacity(2);
    wrapper.insert(
        "ir_version".to_string(),
        serde_json::Value::from("unstable"),
    );
    wrapper.insert("program".to_string(), program_value);
    serde_json::to_string_pretty(&serde_json::Value::Object(wrapper)).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_has_preview_notice() {
        let s = version_string();
        assert!(s.starts_with("electricity "));
        assert!(s.ends_with("(preview)"));
        assert!(s.contains(VERSION));
    }

    #[test]
    fn run_orchestration_refuses_a_document_that_checks_out() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-run-orchestration-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        let doc = dir.join("doc.yml");
        std::fs::write(&config, "{}").unwrap();
        std::fs::write(&doc, "effects: []\n").unwrap();

        let outcome = run_orchestration(&config, &doc, &indexmap::IndexMap::new());
        let message = outcome.to_string();
        // `effects: []` is structurally valid, has no `runtime:`
        // configuration error, and compiles cleanly, so this preview's
        // one unconditional refusal is the only outcome left.
        assert!(
            matches!(outcome, RunOutcome::PreviewRefusal(_)),
            "{outcome:?}"
        );
        assert!(
            message.contains("is a preview and cannot run orchestrations yet"),
            "{message}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_orchestration_reports_a_check_failure_verbatim() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-run-orchestration-test-fail-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        let doc = dir.join("doc.yml");
        std::fs::write(&doc, "").unwrap();

        let outcome = run_orchestration(&config, &doc, &indexmap::IndexMap::new());
        let message = outcome.to_string();
        // The "required property" text past the location is the Rust
        // `jsonschema` crate's own (third-party) wording, not required
        // to match Circuitry's Python `jsonschema` text word for word
        // (DESIGN.md §1/§12) -- only the `"Orchestration validation
        // failed:"` wrapper and the location are Circuitry's own.
        assert!(message.starts_with("Orchestration validation failed:\n  - top level: "));
        assert!(message.contains("required property"), "{message}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_orchestration_passes_e_inputs_to_check_for_run() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-run-orchestration-test-inputs-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        let doc = dir.join("doc.yml");
        std::fs::write(
            &doc,
            "interface:\n  inputs:\n    name:\n      type: string\n      required: true\neffects: []\n",
        )
        .unwrap();

        let outcome = run_orchestration(&config, &doc, &indexmap::IndexMap::new());
        assert!(matches!(outcome, RunOutcome::CheckFailed(_)), "{outcome:?}");
        assert!(
            outcome
                .to_string()
                .contains("missing required input 'name'"),
            "{outcome}"
        );

        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("name".to_string(), "World".to_string());
        let outcome = run_orchestration(&config, &doc, &inputs);
        assert!(
            matches!(outcome, RunOutcome::PreviewRefusal(_)),
            "{outcome:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_options_merges_config_runtime_and_inputs() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-check-options-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        std::fs::write(&config, r#"{"runtime": {"max_concurrency": 3}}"#).unwrap();

        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("name".to_string(), "World".to_string());
        let options = check_options(&config, &inputs);

        assert!(options.skip_preflight);
        assert!(options.trust_document);
        assert_eq!(
            options.inputs.get("name").map(String::as_str),
            Some("World")
        );
        assert!(options.config_runtime.is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_options_tolerates_a_missing_config_file() {
        let options = check_options(
            Path::new("/does/not/exist/config.json"),
            &indexmap::IndexMap::new(),
        );
        assert_eq!(options.config_runtime, None);
    }

    #[test]
    fn parse_inputs_rejects_an_entry_with_no_equals_sign() {
        assert_eq!(
            parse_inputs(&["badtext".to_string()]),
            Err("Invalid -e format: 'badtext' (expected KEY=VALUE)".to_string())
        );
    }

    #[test]
    fn parse_inputs_keeps_a_repeated_key_at_its_first_position_with_the_last_value() {
        let inputs = parse_inputs(&[
            "name=Alice".to_string(),
            "age=30".to_string(),
            "name=Bob".to_string(),
        ])
        .unwrap();
        assert_eq!(inputs.keys().collect::<Vec<_>>(), vec!["name", "age"]);
        assert_eq!(inputs["name"], "Bob");
    }
}
