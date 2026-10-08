//! Lane C: `compile_document`, porting `core/compiler.py`'s
//! `compile_orchestration` — the bare-`input` reference check, declared
//! prompts, the composition checks, then every effect in document
//! order ([`containers`]: `dynamic`/`if`/`loop`; [`leaves`]:
//! `prompt`/`tool`/`use`/`yield`/`reflector`).

pub mod containers;
pub mod leaves;

use crate::{CompileError, DocumentOrigin, compose, not_implemented, prompt_files};
use electricity_bytecode::Program;
use electricity_value::Value;

/// Compiles *document* to a [`Program`], the way
/// `core/compiler.py::compile_orchestration` compiles it to a root
/// `DynamicDefinition`.
///
/// Order matches `compile_orchestration`'s own (issue #408's Scope
/// section): the bare-`input` reference check, declared prompts, the
/// composition checks, then every effect in document order
/// ([`containers`]/[`leaves`]). Concurrency-group references
/// ([`crate::groups::unknown_group_errors`]) and `use` cycles
/// ([`crate::cycles::detect_cycles`]) are checked by the pipeline
/// (`pipeline.rs`, lane B), after a document compiles -- not here:
/// Circuitry's own `cli/runtime_shim.py` checks both against its
/// surfaces' merged runtime config, never inside
/// `compile_orchestration` itself. `declared_prompts`/
/// `check_prompt_composition` are lane D's (`prompt_files.rs`/
/// `compose.rs`); the rest is this lane's own.
///
/// Stub (lane A): fails at the first step below -- declared prompts --
/// until lane D lands, and then at whichever step lane C hasn't filled
/// in yet.
pub fn compile_document(
    document: &Value,
    origin: &DocumentOrigin,
) -> Result<Program, CompileError> {
    let declared_prompts = prompt_files::compile_declared_prompts(document, origin)?;
    compose::check_prompt_composition(document, &declared_prompts)?;
    // Lane C fills in the gap here: every effect in document order,
    // via `containers`/`leaves`, building `root`/`effect_names` below.
    Err(CompileError(not_implemented("compile_document", "C")))
}

#[cfg(test)]
mod tests {
    use super::compile_document;
    use crate::DocumentOrigin;
    use electricity_value::{Dict, Value};
    use std::path::PathBuf;

    /// A document with no `prompts:` key and no `{{>` partial anywhere
    /// passes through both lane D seam stubs, so `compile_document`
    /// reaches its own lane C gap directly -- confirming it no longer
    /// calls `groups::unknown_group_errors`/`cycles::detect_cycles`
    /// itself (those moved to `pipeline.rs`, lane B).
    #[test]
    fn no_longer_fails_on_groups_or_cycles_before_its_own_gap() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(Vec::new()));
        let document = Value::Dict(dict);
        let origin = DocumentOrigin::File {
            document_dir: PathBuf::from("/doc"),
            confinement_root: PathBuf::from("/doc"),
        };

        let err = compile_document(&document, &origin).unwrap_err();

        assert!(
            err.0.contains("compile_document") && err.0.contains("lane C"),
            "expected the lane C `compile_document` gap, got: {}",
            err.0
        );
        assert!(
            !err.0.contains("groups::unknown_group_errors")
                && !err.0.contains("cycles::detect_cycles"),
            "compile_document must no longer call groups/cycles itself, got: {}",
            err.0
        );
    }
}
