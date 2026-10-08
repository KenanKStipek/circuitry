//! Lane C: ports `core/cycle_check.py` — `path:`/`orchestration:`
//! children read the way it reads them (absolute path, then the
//! working directory, then the parent document's directory, no
//! duplicate-key check, an unreadable child treated as empty), with no
//! library lookup, reporting `Cycle: a → b → a`.
//!
//! Called from `pipeline.rs` (lane B), after a document compiles, in
//! both `check_for_run` and `check_report` -- mirroring
//! `cli/runtime_shim.py`'s own two calls to `core/cycle_check.py`'s
//! `detect_cycles`, right after the concurrency-group check
//! ([`crate::groups::unknown_group_errors`]): `use` cycles can only be
//! detected once every `use` effect's `path:`/`orchestration:` child is
//! known, which needs a compiled document, not `compile_document`
//! itself.

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
