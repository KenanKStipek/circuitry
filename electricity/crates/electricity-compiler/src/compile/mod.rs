//! Lane C: `compile_document`, porting `core/compiler.py`'s
//! `compile_orchestration` — the bare-`input` reference check, declared
//! prompts, the composition checks, then every effect in document
//! order ([`containers`]: `dynamic`/`if`/`loop`; [`leaves`]:
//! `prompt`/`tool`/`use`/`yield`/`reflector`).

pub mod containers;
pub mod leaves;

use crate::{CompileError, DocumentOrigin, not_implemented};
use electricity_bytecode::Program;
use electricity_value::Value;

/// Compiles *document* to a [`Program`], the way
/// `core/compiler.py::compile_orchestration` compiles it to a root
/// `DynamicDefinition`.
///
/// Stub (lane A): always fails until lane C lands.
pub fn compile_document(
    document: &Value,
    origin: &DocumentOrigin,
) -> Result<Program, CompileError> {
    let _ = (document, origin);
    Err(CompileError(not_implemented("compile_document", "C")))
}
