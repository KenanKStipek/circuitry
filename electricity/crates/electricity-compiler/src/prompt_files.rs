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
/// Seam stub (lane A): a document with no top-level `prompts` key
/// passes through with no declared prompts (`Ok(IndexMap::new())`),
/// so lane C can test compilation of documents that don't use this
/// feature without waiting for lane D. A document that *does* declare
/// `prompts:` still fails with the lane D marker until lane D lands.
pub(crate) fn compile_declared_prompts(
    document: &Value,
    origin: &DocumentOrigin,
) -> Result<IndexMap<String, String>, CompileError> {
    let _ = origin;
    let has_prompts_key = document
        .as_dict()
        .is_some_and(|dict| dict.contains_key(&Value::Str("prompts".to_string())));
    if !has_prompts_key {
        return Ok(IndexMap::new());
    }
    Err(CompileError(crate::not_implemented(
        "prompt_files::compile_declared_prompts",
        "D",
    )))
}

#[cfg(test)]
mod tests {
    use super::compile_declared_prompts;
    use crate::DocumentOrigin;
    use electricity_value::{Dict, Value};
    use indexmap::IndexMap;
    use std::path::PathBuf;

    fn origin() -> DocumentOrigin {
        DocumentOrigin::File {
            document_dir: PathBuf::from("/doc"),
            confinement_root: PathBuf::from("/doc"),
        }
    }

    #[test]
    fn document_with_no_prompts_key_passes_through() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(Vec::new()));
        let document = Value::Dict(dict);

        let result = compile_declared_prompts(&document, &origin());

        assert_eq!(result, Ok(IndexMap::new()));
    }

    #[test]
    fn document_with_a_prompts_key_fails_with_the_lane_d_marker() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("prompts".to_string()), Value::Dict(Dict::new()));
        let document = Value::Dict(dict);

        let err = compile_declared_prompts(&document, &origin()).unwrap_err();

        assert!(err.0.contains("lane D"), "unexpected error: {}", err.0);
    }
}
