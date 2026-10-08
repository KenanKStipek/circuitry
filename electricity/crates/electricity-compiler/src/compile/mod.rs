//! Lane C: `compile_document`, porting `core/compiler.py`'s
//! `compile_orchestration` — the bare-`input` reference check, declared
//! prompts, the composition checks, then every effect in document
//! order ([`containers`]: `dynamic`/`if`/`loop`; [`leaves`]:
//! `prompt`/`tool`/`use`/`yield`/`reflector`).

pub mod containers;
pub mod leaves;

use crate::{CompileError, DocumentOrigin, compose, cycles, groups, not_implemented, prompt_files};
use electricity_bytecode::Program;
use electricity_value::Value;

/// Compiles *document* to a [`Program`], the way
/// `core/compiler.py::compile_orchestration` compiles it to a root
/// `DynamicDefinition`.
///
/// Order matches `compile_orchestration`'s own (issue #408's Scope
/// section): the bare-`input` reference check, declared prompts, the
/// composition checks, then every effect in document order
/// ([`containers`]/[`leaves`]), then concurrency-group references
/// ([`crate::groups`]) and `use` cycles ([`crate::cycles`]).
/// `declared_prompts`/`check_prompt_composition` are lane D's
/// (`prompt_files.rs`/`compose.rs`); the rest is this lane's own.
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
    groups::unknown_group_errors(document, &[])?;
    cycles::detect_cycles(document, origin)?;
    Err(CompileError(not_implemented("compile_document", "C")))
}
