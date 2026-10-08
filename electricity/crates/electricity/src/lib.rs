//! Preview skeleton of the electricity library crate.
//!
//! This release ships no VM, tool, or adapter implementation (see
//! `../../DESIGN.md`); [`run_orchestration`] always fails with
//! [`PreviewUnsupported`]. [`dump_ir`] is the one exception
//! (issue #408's CLI section): an unstable debugging aid that runs the
//! compiler's `check_for_run` and prints the result, wired all the way
//! through even though `electricity-compiler` itself is still a lane A
//! stub -- see that crate's docs for which lane fills in each piece.

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

/// Always returns `Err(PreviewUnsupported)`: this preview has no compiler or VM.
pub fn run_orchestration() -> Result<(), PreviewUnsupported> {
    Err(PreviewUnsupported)
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
    };
    let program = electricity_compiler::check_for_run(orchestration_path, &options)
        .map_err(|err| err.to_string())?;
    let wrapper = serde_json::json!({
        "ir_version": "unstable",
        "program": program,
    });
    serde_json::to_string_pretty(&wrapper).map_err(|err| err.to_string())
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
    fn run_orchestration_is_unsupported() {
        let err = run_orchestration().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("cannot run orchestrations yet"));
        assert!(message.contains("cof run"));
    }
}
