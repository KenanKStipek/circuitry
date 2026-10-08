//! Lane D: ports `core/prompt_files.py` — reading a `{file: ...}`
//! prompt source at compile time: literal/relative paths with no
//! `{{ }}`, confinement to the nearest `circuitry.config.json`/
//! `config.json` (checked after following symlinks, non-strict
//! resolve, before the existence check), missing/not-a-regular-file/
//! over-1-MiB/not-UTF-8/unreadable errors, universal-newline
//! translation, and the refusal for [`crate::DocumentOrigin::Generated`].
//!
//! Called first from `compile::compile_document` (lane C), matching
//! `core/compiler.py::compile_orchestration`'s order: declared prompts
//! read before the composition checks ([`crate::compose`]) before any
//! effect compiles.

use crate::{CompileError, DocumentOrigin};
use electricity_value::Value;
use indexmap::IndexMap;

/// The top-level `prompts:` map, read into raw text with any `{file:
/// ...}` source already resolved -- `Program.prompts`.
///
/// Stub (lane A): always fails until lane D lands.
pub(crate) fn compile_declared_prompts(
    document: &Value,
    origin: &DocumentOrigin,
) -> Result<IndexMap<String, String>, CompileError> {
    let _ = (document, origin);
    Err(CompileError(crate::not_implemented(
        "prompt_files::compile_declared_prompts",
        "D",
    )))
}
