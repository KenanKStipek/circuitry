//! Lane C: ports `core/cycle_check.py` — `path:`/`orchestration:`
//! children read the way it reads them (absolute path, then the
//! working directory, then the parent document's directory, no
//! duplicate-key check, an unreadable child treated as empty), with no
//! library lookup, reporting `Cycle: a → b → a`.
//!
//! Called from `compile::compile_document` (lane C, same lane), last --
//! `use` cycles can only be detected once every `use` effect's
//! `path:`/`orchestration:` child is known.

use crate::{CompileError, DocumentOrigin};
use electricity_value::Value;

/// A `Cycle: a → b → a` error among *document*'s `use` effects, if any.
///
/// Stub (lane A): always fails until lane C lands.
pub(crate) fn detect_cycles(document: &Value, origin: &DocumentOrigin) -> Result<(), CompileError> {
    let _ = (document, origin);
    Err(CompileError(crate::not_implemented(
        "cycles::detect_cycles",
        "C",
    )))
}
