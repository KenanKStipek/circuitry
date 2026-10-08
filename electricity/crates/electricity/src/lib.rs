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

/// Runs *orchestration_path* the way `electricity <config.json> <doc> ...`
/// does (issue #408's CLI section): [`electricity_compiler::check_for_run`]
/// first, trusting the document and skipping preflight, with
/// *config_path*'s own `runtime:` block (if the file exists and parses)
/// merged under the document's own, key by key -- then, on success,
/// still this preview's one refusal, since there is no VM yet.
///
/// A [`RunOutcome::CheckFailed`] carries [`electricity_compiler::check_for_run`]'s
/// own error text verbatim -- the exact text the CLI writes to stderr on a
/// check failure (issue #408's CLI section: "On failure: exactly the error
/// text on stderr, exit 1"). [`RunOutcome::PreviewRefusal`] is the
/// unconditional "On success: keep the current preview refusal" branch.
pub fn run_orchestration(config_path: &Path, orchestration_path: &Path) -> RunOutcome {
    let options = electricity_compiler::CheckOptions {
        skip_preflight: true,
        trust_document: true,
        config_runtime: config_runtime_block(config_path),
    };
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
/// Runs `electricity_compiler::check_for_run` first; its `Err` becomes
/// this function's `Err`, with the exact text `--dump-ir` writes to
/// stderr on failure (the same text a plain run of the same document
/// would report). Currently always `Err`: `check_for_run` is a lane A
/// stub until lane B lands.
pub fn dump_ir(orchestration_path: &Path) -> Result<String, String> {
    let options = electricity_compiler::CheckOptions {
        skip_preflight: true,
        trust_document: true,
        config_runtime: None,
    };
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

        let outcome = run_orchestration(&config, &doc);
        let message = outcome.to_string();
        // `effects: []` is structurally valid and has no `runtime:`
        // configuration error, so it reaches `compile_document` --
        // still a lane C stub today, hence `CheckFailed`, not
        // `PreviewRefusal`, until that lane lands.
        assert!(matches!(outcome, RunOutcome::CheckFailed(_)), "{outcome:?}");
        assert!(message.contains("not implemented in lane"), "{message}");

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

        let outcome = run_orchestration(&config, &doc);
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
}
