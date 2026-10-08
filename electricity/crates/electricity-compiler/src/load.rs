//! Lane B: `load_document`, porting `cli/orchestration_loader.py`.
//!
//! The suffix picks the format (`.yml`/`.yaml` -> `electricity-yaml`,
//! `.json` -> `electricity-json`, `.toon` refused); text is read with
//! universal-newline translation, and a duplicate-key message is
//! prefixed with the path as given.

use crate::{CompileError, not_implemented};
use electricity_value::Value;
use std::path::Path;

/// Loads *path* the way `cli/orchestration_loader.py` does.
///
/// Stub (lane A): always fails until lane B lands.
pub fn load_document(path: &Path) -> Result<Value, CompileError> {
    let _ = path;
    Err(CompileError(not_implemented("load_document", "B")))
}
