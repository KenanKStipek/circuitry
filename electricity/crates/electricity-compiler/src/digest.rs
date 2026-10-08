//! Lane D: ports `core/prompt_compose.py::document_content_digest`
//! byte for byte -- the document bytes plus, for each referenced prompt
//! file, its relative-path label, its 8-byte length and its bytes
//! (#407's single algorithm).
//!
//! Called from `pipeline.rs` (lane B), which has the resolved path and
//! has already read the document's own bytes by the time it has a
//! compiled [`electricity_bytecode::Program`] to attach a
//! [`electricity_bytecode::DocumentInfo`] to -- `compile_document`
//! itself never sees raw bytes or the path as given (see
//! `electricity_bytecode::Program::document`'s doc comment).

use crate::CompileError;
use electricity_value::Value;
use std::path::Path;

/// `document_content_digest(resolved_path, document, confinement_root)`
/// -- the document's bytes plus every `{file: ...}` prompt source it
/// references, hashed the way `core/prompt_compose.py` hashes them.
///
/// Stub (lane A): always fails until lane D lands.
pub(crate) fn document_content_digest(
    resolved_path: &Path,
    document: &Value,
    confinement_root: &Path,
) -> Result<String, CompileError> {
    let _ = (resolved_path, document, confinement_root);
    Err(CompileError(crate::not_implemented(
        "digest::document_content_digest",
        "D",
    )))
}
