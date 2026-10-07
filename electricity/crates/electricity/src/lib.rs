//! Preview skeleton of the electricity library crate.
//!
//! This release ships no compiler, bytecode, VM, tool, or adapter
//! implementation (see `../../DESIGN.md`); [`run_orchestration`] always
//! fails with [`PreviewUnsupported`].

use std::fmt;

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
