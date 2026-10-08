//! Lane D: ports `core/prompt_compose.py`'s composition checks — each
//! fragment's syntax, set-delimiter tags refused, unknown names
//! resolved scope-aware, collisions between declared prompts and
//! effects, references to non-text effects, cycles among declared
//! prompts, and the `type: yield` compile step.
//!
//! Called from `compile::compile_document` (lane C) right after
//! [`crate::prompt_files::compile_declared_prompts`], matching
//! `core/compiler.py::compile_orchestration`'s order: the composition
//! checks run before any effect compiles.

use crate::CompileError;
use electricity_value::Value;
use indexmap::IndexMap;

/// The compile-time half of #406's composition checks against
/// *document* and its already-read *declared_prompts*.
///
/// Stub (lane A): always fails until lane D lands.
pub(crate) fn check_prompt_composition(
    document: &Value,
    declared_prompts: &IndexMap<String, String>,
) -> Result<(), CompileError> {
    let _ = (document, declared_prompts);
    Err(CompileError(crate::not_implemented(
        "compose::check_prompt_composition",
        "D",
    )))
}
