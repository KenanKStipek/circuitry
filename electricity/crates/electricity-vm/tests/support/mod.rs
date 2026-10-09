// Shared across several test *binaries* (one `mod support;` per file,
// cargo's own integration-test convention) -- each only uses the subset
// of these builders its own scenarios need, so an unused one here is
// expected, not a real dead-code defect in any single binary.
#![allow(dead_code)]

//! Shared builders for the lane B2 (interpreter) container tests --
//! `Op`/`Region` fixtures a hand-written test document needs, plus a
//! [`RecordingObserver`] that remembers every hook call in order.
//!
//! `exec::tool::execute_tool` (lane C, now final) runs the real `json`
//! tool from [`json_registry`] -- every `tool` leaf built here
//! ([`tool_leaf`]) is `mode: parse` against a fixed, deterministically
//! malformed `input:`, so it always fails the same way
//! ([`TOOL_FAIL_TEXT`]), real tool dispatch (its own `effect_start`/
//! `effect_complete`/`write` hooks included) and all. That lets these
//! tests exercise chain/tree ordering, wrapping, `on_error`, `finally`,
//! dispatch and cancellation without depending on anything lane C's
//! `json` tool doesn't itself need (a network call, `use`, a real
//! model). A leaf that must *succeed* uses an empty nested `dynamic`
//! instead (trivially `Ok(())` -- `execute_root_runs_an_empty_
//! document_to_completion` in `electricity-vm`'s own `lib.rs` proves
//! that).

use electricity_bytecode::{
    Condition, DocumentInfo, EffectPath, LeafKind, NodeKind, OnError, Op, ParamNode, Program,
    Region, RetryPolicy, ToolOp,
};
use electricity_tools::ToolRegistry;
use electricity_value::Value;
use electricity_vm::{Limiter, RunContext, RunObserver};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Wraps *root* (always named `prime`) in a minimal [`Program`] --
/// `execute_root`, the only `pub` entry point this crate exposes, is
/// what every one of these tests actually drives.
pub fn program(root: Op) -> Program {
    Program {
        root,
        prompts: IndexMap::new(),
        effect_names: BTreeSet::new(),
        document: Some(DocumentInfo {
            path_as_given: "doc.yml".to_string(),
            resolved_directory: PathBuf::new(),
            confinement_root: PathBuf::new(),
            digest: None,
        }),
        runtime_block: None,
        interface: None,
        adapter: None,
        model: None,
    }
}

/// A fresh [`ToolRegistry`] with the real `json` tool registered --
/// every test that runs a [`tool_leaf`] (or any other real `tool`
/// effect) needs one of these, not a bare `ToolRegistry::new()`.
pub fn json_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(electricity_tools::json::JsonTool));
    registry
}

/// `json`'s own `"json: parse failed: {err}"` wrap (`plugins/json.py`)
/// around [`tool_leaf`]'s fixed malformed `input:` (`"not json"`) --
/// third-party JSON-decoder text (DESIGN.md §1/§12: only has to fail at
/// the same point CPython's own decoder would, not match it verbatim),
/// but deterministic for this one fixed string, which is all any test
/// here needs.
pub const TOOL_FAIL_TEXT: &str = "json: parse failed: Expecting value: char 0";

/// A `tool` leaf -- real `json`-tool dispatch (through [`json_registry`]),
/// `mode: parse` against the fixed malformed `input:` that always fails
/// with [`TOOL_FAIL_TEXT`].
pub fn tool_leaf(path: EffectPath, name: &str) -> Op {
    let mut params = IndexMap::new();
    params.insert(
        Value::Str("mode".to_string()),
        electricity_bytecode::ParamNode::Literal(Value::Str("parse".to_string())),
    );
    params.insert(
        Value::Str("input".to_string()),
        electricity_bytecode::ParamNode::Literal(Value::Str("not json".to_string())),
    );
    json_tool_leaf(path, name, params)
}

/// A `json` tool leaf with *params* exactly as given -- [`tool_leaf`]'s
/// own general form, for a test that needs a specific `mode`/`input`
/// (a template param, say) rather than the fixed always-fails shape.
pub fn json_tool_leaf(path: EffectPath, name: &str, params: IndexMap<Value, ParamNode>) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
            provider: "json".to_string(),
            params: electricity_bytecode::ParamNode::Map(params),
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }))),
        on_error: OnError::Fail,
        labels: None,
        enabled: true,
    }
}

/// A chain-flow `dynamic` with no `finally:`.
pub fn chain_dynamic(path: EffectPath, name: &str, on_error: OnError, ops: Vec<Op>) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::Block {
            ops,
            overlay: false,
        }),
        on_error,
        labels: None,
        enabled: true,
    }
}

/// A tree-flow `dynamic` with no `finally:`.
pub fn tree_dynamic(
    path: EffectPath,
    name: &str,
    on_error: OnError,
    max_concurrency: Option<u32>,
    stop_on_error: bool,
    branches: Vec<Op>,
) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::Parallel {
            branches,
            max_concurrency,
            stop_on_error,
        }),
        on_error,
        labels: None,
        enabled: true,
    }
}

/// A chain-flow `dynamic` with a `finally:` block.
pub fn dynamic_with_finally(
    path: EffectPath,
    name: &str,
    on_error: OnError,
    body_ops: Vec<Op>,
    finally_ops: Vec<Op>,
) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::TryFinally {
            body: Box::new(Region::Block {
                ops: body_ops,
                overlay: false,
            }),
            finally: Box::new(Region::Block {
                ops: finally_ops,
                overlay: false,
            }),
        }),
        on_error,
        labels: None,
        enabled: true,
    }
}

/// A CEL `if`, named or transparent depending on *name*.
pub fn cel_if(
    path: EffectPath,
    name: Option<&str>,
    expr: &str,
    on_error: OnError,
    then_ops: Vec<Op>,
    else_ops: Option<Vec<Op>>,
) -> Op {
    Op {
        path,
        name: name.map(str::to_string),
        kind: NodeKind::Control(Region::If {
            cond: Condition::Cel {
                expr: expr.to_string(),
                strict: false,
            },
            then_: Box::new(Region::Block {
                ops: then_ops,
                overlay: true,
            }),
            else_: else_ops.map(|ops| Box::new(Region::Block { ops, overlay: true })),
            threshold: 0.5,
        }),
        on_error,
        labels: None,
        enabled: true,
    }
}

/// Every hook [`RecordingObserver`] saw, in call order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Start(String),
    Complete(String, Option<String>),
    Dispatch(String, usize, usize),
    Write,
}

#[derive(Default)]
pub struct RecordingObserver {
    events: RefCell<Vec<Event>>,
}

impl RecordingObserver {
    pub fn new() -> Self {
        RecordingObserver::default()
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.borrow().clone()
    }
}

impl RunObserver for RecordingObserver {
    fn effect_start(&self, path: &EffectPath) {
        self.events
            .borrow_mut()
            .push(Event::Start(path.to_string()));
    }

    fn effect_complete(&self, path: &EffectPath, error: Option<&str>) {
        self.events
            .borrow_mut()
            .push(Event::Complete(path.to_string(), error.map(str::to_string)));
    }

    fn dispatch(&self, path: &EffectPath, branches: usize, concurrency: usize) {
        self.events
            .borrow_mut()
            .push(Event::Dispatch(path.to_string(), branches, concurrency));
    }

    fn write(&self) {
        self.events.borrow_mut().push(Event::Write);
    }
}

/// A [`RunObserver`] that cancels *token* the moment *target*'s own
/// `effect_start` fires -- lets a test drive a real mid-run
/// cancellation deterministically, with no real thread/signal/sleep
/// involved, by picking exactly the path whose own start should trigger
/// it. *token* must be the same [`electricity_vm::CancellationToken`]
/// the run itself was started with.
pub struct CancelOnStart<'a> {
    pub inner: RecordingObserver,
    pub token: &'a electricity_vm::CancellationToken,
    pub target: &'a str,
}

impl RunObserver for CancelOnStart<'_> {
    fn effect_start(&self, path: &EffectPath) {
        self.inner.effect_start(path);
        if path.to_string() == self.target {
            self.token.request(2);
        }
    }

    fn effect_complete(&self, path: &EffectPath, error: Option<&str>) {
        self.inner.effect_complete(path, error);
    }

    fn dispatch(&self, path: &EffectPath, branches: usize, concurrency: usize) {
        self.inner.dispatch(path, branches, concurrency);
    }
}

/// A bare-minimum [`RunContext`] -- an unconfigured registry/limiter,
/// `model: ""`, `adapter: "_noop"` (run-wiring step 15 of issue #431's
/// table), `dry_run: false`.
pub fn run_ctx<'a>(
    registry: &'a electricity_tools::ToolRegistry,
    limiter: &'a Limiter,
) -> RunContext<'a> {
    RunContext {
        registry,
        limiter,
        model: "",
        adapter: "_noop",
        runtime_config: EMPTY_RUNTIME_CONFIG,
        dry_run: false,
    }
}

pub static EMPTY_RUNTIME_CONFIG: &Value = &Value::None;
