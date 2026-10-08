//! Lane C: `compile_document`, porting `core/compiler.py`'s
//! `compile_orchestration` — the bare-`input` reference check, declared
//! prompts, the composition checks, then every effect in document
//! order ([`containers`]: `dynamic`/`if`/`loop`; [`leaves`]:
//! `prompt`/`tool`/`use`/`yield`/`reflector`).

pub mod coerce;
pub mod containers;
pub mod expect;
pub mod leaves;
pub mod names;
pub mod params;
pub mod templates;

use crate::{CompileError, DocumentOrigin, compose, prompt_files};
use electricity_bytecode::path::{EffectPath, LoopId};
use electricity_bytecode::{NodeKind, OnError, Op, Program, Region};
use electricity_value::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Per-document compile-time state threaded through every container/
/// leaf-compiling function: today, just the monotonic counter behind a
/// named loop's [`LoopId`] (DESIGN.md §5.1 -- "assigned in document
/// order by the compiler").
pub(crate) struct Ctx {
    next_loop_id: u32,
}

impl Ctx {
    fn new() -> Self {
        Ctx { next_loop_id: 0 }
    }

    pub(crate) fn next_loop_id(&mut self) -> LoopId {
        let id = LoopId(self.next_loop_id);
        self.next_loop_id += 1;
        id
    }
}

fn dict_get<'a>(document: &'a Value, key: &str) -> Option<&'a Value> {
    document.as_dict()?.get(&Value::Str(key.to_string()))
}

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
/// (`pipeline.rs`, lane B), after a document compiles, not here:
/// Circuitry's own `cli/runtime_shim.py` checks both against its
/// surfaces' merged runtime config, never inside
/// `compile_orchestration` itself. `declared_prompts`/
/// `check_prompt_composition` are lane D's (`prompt_files.rs`/
/// `compose.rs`); the rest is this lane's own.
pub fn compile_document(
    document: &Value,
    origin: &DocumentOrigin,
) -> Result<Program, CompileError> {
    let declared_prompts = prompt_files::compile_declared_prompts(document, origin)?;
    compose::check_prompt_composition(document, &declared_prompts)?;

    crate::state_ns::validate_bare_input_refs(document)?;

    let root_path = EffectPath::root();
    let mut ctx = Ctx::new();

    let top_effects = dict_get(document, "effects");
    let effects = if top_effects.is_some_and(crate::state_ns::is_truthy) {
        top_effects
    } else {
        dict_get(document, "steps")
    };
    let empty = Value::List(Vec::new());
    let effects = effects.unwrap_or(&empty);

    let mut seen_names: BTreeMap<String, String> = BTreeMap::new();
    let compiled_effects = containers::compile_effects_in_scope(
        &mut ctx,
        effects,
        &root_path,
        "prime",
        "prime.effects",
        &BTreeSet::new(),
        &mut seen_names,
    )?;

    let finally_raw = dict_get(document, "finally");
    let finally_present = finally_raw.is_some_and(crate::state_ns::is_truthy);
    let compiled_finally = if finally_present {
        containers::compile_effects_in_scope(
            &mut ctx,
            finally_raw.unwrap(),
            &root_path,
            "prime",
            "prime.finally",
            &BTreeSet::new(),
            &mut seen_names,
        )?
    } else {
        Vec::new()
    };

    let flow = containers::normalize_flow(
        coerce::first_truthy(&[dict_get(document, "flow"), dict_get(document, "strategy")])
            .filter(|v| crate::state_ns::is_truthy(v))
            .map(Value::py_str)
            .as_deref(),
    )?;

    let body_region = match flow {
        electricity_bytecode::LoopFlow::Tree => Region::Parallel {
            branches: compiled_effects,
            max_concurrency: None,
            stop_on_error: false,
        },
        electricity_bytecode::LoopFlow::Chain => Region::Block {
            ops: compiled_effects,
            overlay: false,
        },
    };
    let region = if compiled_finally.is_empty() {
        body_region
    } else {
        Region::TryFinally {
            body: Box::new(body_region),
            finally: Box::new(Region::Block {
                ops: compiled_finally,
                overlay: false,
            }),
        }
    };

    let root = Op {
        path: root_path,
        name: Some("prime".to_string()),
        kind: NodeKind::Control(region),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    };

    let runtime_block = dict_get(document, "runtime").cloned();
    let interface = dict_get(document, "interface").cloned();
    let adapter = dict_get(document, "adapter")
        .and_then(Value::as_str)
        .map(str::to_string);
    let model = dict_get(document, "model")
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(Program {
        root,
        prompts: declared_prompts,
        effect_names: compose::all_effect_names(document),
        document: None,
        runtime_block,
        interface,
        adapter,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::compile_document;
    use crate::DocumentOrigin;
    use electricity_value::{Dict, Value};
    use std::path::PathBuf;

    fn origin() -> DocumentOrigin {
        DocumentOrigin::File {
            document_dir: PathBuf::from("/doc"),
            confinement_root: PathBuf::from("/doc"),
        }
    }

    #[test]
    fn an_empty_effects_list_compiles_to_an_empty_root() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(Vec::new()));
        let document = Value::Dict(dict);

        let program = compile_document(&document, &origin()).unwrap();

        assert_eq!(program.root.name.as_deref(), Some("prime"));
        match &program.root.kind {
            electricity_bytecode::NodeKind::Control(electricity_bytecode::Region::Block {
                ops,
                overlay,
            }) => {
                assert!(ops.is_empty());
                assert!(!overlay);
            }
            other => panic!("expected an empty root block, got {other:?}"),
        }
    }

    #[test]
    fn a_single_tool_effect_compiles() {
        let mut tool = Dict::new();
        tool.insert(
            Value::Str("type".to_string()),
            Value::Str("tool".to_string()),
        );
        tool.insert(
            Value::Str("name".to_string()),
            Value::Str("fetch".to_string()),
        );
        tool.insert(
            Value::Str("provider".to_string()),
            Value::Str("shell".to_string()),
        );

        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(tool)]),
        );
        let document = Value::Dict(dict);

        let program = compile_document(&document, &origin()).unwrap();
        match &program.root.kind {
            electricity_bytecode::NodeKind::Control(electricity_bytecode::Region::Block {
                ops,
                ..
            }) => {
                assert_eq!(ops.len(), 1);
                assert_eq!(ops[0].name.as_deref(), Some("fetch"));
            }
            other => panic!("expected a block, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_names_in_the_same_scope_are_rejected() {
        let mut a = Dict::new();
        a.insert(
            Value::Str("type".to_string()),
            Value::Str("tool".to_string()),
        );
        a.insert(Value::Str("name".to_string()), Value::Str("x".to_string()));
        a.insert(
            Value::Str("provider".to_string()),
            Value::Str("shell".to_string()),
        );
        let mut b = a.clone();
        b.insert(
            Value::Str("provider".to_string()),
            Value::Str("http".to_string()),
        );

        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(a), Value::Dict(b)]),
        );
        let document = Value::Dict(dict);

        let err = compile_document(&document, &origin()).unwrap_err();
        assert!(err.0.contains("Duplicate effect name 'x' in scope 'prime'"));
    }

    #[test]
    fn missing_type_is_an_error() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(Dict::new())]),
        );
        let document = Value::Dict(dict);

        let err = compile_document(&document, &origin()).unwrap_err();
        assert!(err.0.contains("is missing required field 'type'"));
    }

    #[test]
    fn program_carries_effect_names_and_top_level_fields() {
        let mut tool = Dict::new();
        tool.insert(
            Value::Str("type".to_string()),
            Value::Str("tool".to_string()),
        );
        tool.insert(
            Value::Str("name".to_string()),
            Value::Str("fetch".to_string()),
        );
        tool.insert(
            Value::Str("provider".to_string()),
            Value::Str("shell".to_string()),
        );

        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Dict(tool)]),
        );
        dict.insert(
            Value::Str("adapter".to_string()),
            Value::Str("ollama".to_string()),
        );
        dict.insert(
            Value::Str("model".to_string()),
            Value::Str("llama3".to_string()),
        );
        let document = Value::Dict(dict);

        let program = compile_document(&document, &origin()).unwrap();
        assert_eq!(
            program.effect_names,
            ["fetch".to_string()].into_iter().collect()
        );
        assert_eq!(program.adapter.as_deref(), Some("ollama"));
        assert_eq!(program.model.as_deref(), Some("llama3"));
    }

    #[test]
    fn no_longer_fails_on_groups_or_cycles() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(Vec::new()));
        let document = Value::Dict(dict);

        let program = compile_document(&document, &origin()).unwrap();
        let _ = program;
    }
}
