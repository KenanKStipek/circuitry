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
    Region, RetryPolicy, ToolOp, UseOp, UseSource,
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
/// the third-party JSON-decoder text after it is byte-identical to
/// CPython's own `json.JSONDecodeError` text (issue #442), deterministic
/// for this one fixed string, which is all any test here needs.
pub const TOOL_FAIL_TEXT: &str = "json: parse failed: Expecting value: line 1 column 1 (char 0)";

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

/// A `use` leaf whose own inline child names a `prompt` effect --
/// refused every time (`Supported::prompt` stays `false` until lane G
/// lands), so `exec::use_::execute_use`'s own real `first_unsupported`
/// check against the freshly-compiled child produces a real
/// `electricity_vm::VmError::Refused`, exercising that variant's
/// propagation through a real, compiled `Op` tree without depending on
/// a filesystem child or a test-only sentinel (M1 lane I, issue #469 --
/// replaces the deleted `__test_only_refused__` sentinel in
/// `exec/use_.rs`).
pub fn refused_use_leaf(path: EffectPath, name: &str, on_error: OnError) -> Op {
    let inline_yaml = "effects:\n  - type: prompt\n    name: unsupported\n    template: hi\n";
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Use(UseOp {
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                inline_yaml.to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: None,
            outputs: None,
            validate: true,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
        }))),
        on_error,
        labels: None,
        enabled: true,
    }
}

/// A `use` leaf, every field explicit -- the general builder M1 lane
/// I's own `tests/use_effect.rs` needs, [`refused_use_leaf`]'s own
/// general form.
#[allow(clippy::too_many_arguments)]
pub fn use_leaf(
    path: EffectPath,
    name: &str,
    source: UseSource,
    inputs: Option<ParamNode>,
    outputs: Option<IndexMap<String, String>>,
    validate: bool,
    retries: RetryPolicy,
    expect: Option<electricity_bytecode::ExpectCondition>,
    on_error: OnError,
) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Leaf(Box::new(LeafKind::Use(UseOp {
            source,
            inputs,
            outputs,
            validate,
            retries,
            expect,
            description: None,
        }))),
        on_error,
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

/// A tree-flow `dynamic` with a `finally:` block.
pub fn tree_dynamic_with_finally(
    path: EffectPath,
    name: &str,
    on_error: OnError,
    max_concurrency: Option<u32>,
    stop_on_error: bool,
    branches: Vec<Op>,
    finally_ops: Vec<Op>,
) -> Op {
    Op {
        path,
        name: Some(name.to_string()),
        kind: NodeKind::Control(Region::TryFinally {
            body: Box::new(Region::Parallel {
                branches,
                max_concurrency,
                stop_on_error,
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

/// A `mode: model` `if` -- `execute_conditional`'s own `decide_and_run`
/// routes this through `condition_model::evaluate_model_condition`
/// (lane G2, #513), real since that lane landed.
pub fn model_if(path: EffectPath, name: Option<&str>, template: &str, on_error: OnError) -> Op {
    Op {
        path,
        name: name.map(str::to_string),
        kind: NodeKind::Control(Region::If {
            cond: Condition::Model {
                template: electricity_bytecode::TemplateText::new(
                    template,
                    false,
                    electricity_bytecode::Escape::None,
                ),
            },
            then_: Box::new(Region::Block {
                ops: vec![],
                overlay: true,
            }),
            else_: None,
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
    /// [`RunObserver::graft_child_write`] -- the node path, and the
    /// grafted snapshot rendered through its own `Debug` text (the
    /// snapshot's exact shape is a child `Store::saved` tree, not
    /// worth giving `Event` its own `Value`-carrying variant just for
    /// the handful of tests that look at it at all).
    Graft(String, String),
    /// [`RunObserver::clear_graft`].
    ClearGraft(String),
    /// [`RunObserver::record_totals`] -- the completing effect's own
    /// `meta` snapshot, rendered through `Debug` for the same reason
    /// [`Event::Graft`] does.
    Totals(String),
    /// [`RunObserver::register_tree_branches`] -- the container's own
    /// node path, and how many branches were registered (never the
    /// branch `NodeRef`s themselves -- nothing in this crate's own
    /// test suite needs to inspect their live content through an
    /// `Event`; a live-state-shape test belongs in `electricity` (lib),
    /// which actually owns a `LiveStateMirror`).
    RegisterTreeBranches(String, usize),
    /// [`RunObserver::clear_tree_branches`].
    ClearTreeBranches(String),
}

pub struct RecordingObserver {
    events: RefCell<Vec<Event>>,
    next_instance: std::cell::Cell<electricity_vm::observer::InstanceId>,
    /// `true` by default -- every existing test here that asserts a
    /// `Event::Graft`/`Event::ClearGraft` pair happened needs
    /// `exec::use_::ChildObserver::write` to actually take one (R4 on
    /// PR #472's review gates it on this). [`Self::with_child_snapshots`]
    /// turns it off for a test that specifically checks the *opposite*.
    wants_child_snapshots: std::cell::Cell<bool>,
}

impl Default for RecordingObserver {
    fn default() -> Self {
        RecordingObserver {
            events: RefCell::new(Vec::new()),
            next_instance: std::cell::Cell::new(0),
            wants_child_snapshots: std::cell::Cell::new(true),
        }
    }
}

impl RecordingObserver {
    pub fn new() -> Self {
        RecordingObserver::default()
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.borrow().clone()
    }

    pub fn with_child_snapshots(self, wants: bool) -> Self {
        self.wants_child_snapshots.set(wants);
        self
    }
}

impl RunObserver for RecordingObserver {
    fn effect_start(&self, path: &EffectPath) -> electricity_vm::observer::InstanceId {
        self.events
            .borrow_mut()
            .push(Event::Start(path.to_string()));
        let instance = self.next_instance.get() + 1;
        self.next_instance.set(instance);
        instance
    }

    fn effect_complete(
        &self,
        path: &EffectPath,
        _instance: electricity_vm::observer::InstanceId,
        error: Option<&str>,
        _completed_at_is_truthy: bool,
    ) {
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

    fn graft_child_write(&self, node_path: &EffectPath, snapshot: &Value) {
        self.events
            .borrow_mut()
            .push(Event::Graft(node_path.to_string(), format!("{snapshot:?}")));
    }

    fn clear_graft(&self, node_path: &EffectPath) {
        self.events
            .borrow_mut()
            .push(Event::ClearGraft(node_path.to_string()));
    }

    fn register_tree_branches(&self, node_path: &EffectPath, branches: &[electricity_vm::NodeRef]) {
        self.events.borrow_mut().push(Event::RegisterTreeBranches(
            node_path.to_string(),
            branches.len(),
        ));
    }

    fn clear_tree_branches(&self, node_path: &EffectPath, _branches: &[electricity_vm::NodeRef]) {
        self.events
            .borrow_mut()
            .push(Event::ClearTreeBranches(node_path.to_string()));
    }

    fn record_totals(&self, meta: &Value) {
        self.events
            .borrow_mut()
            .push(Event::Totals(format!("{meta:?}")));
    }

    fn wants_child_snapshots(&self) -> bool {
        self.wants_child_snapshots.get()
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
    fn effect_start(&self, path: &EffectPath) -> electricity_vm::observer::InstanceId {
        let instance = self.inner.effect_start(path);
        if path.to_string() == self.target {
            self.token.request(2);
        }
        instance
    }

    fn effect_complete(
        &self,
        path: &EffectPath,
        instance: electricity_vm::observer::InstanceId,
        error: Option<&str>,
        completed_at_is_truthy: bool,
    ) {
        self.inner
            .effect_complete(path, instance, error, completed_at_is_truthy);
    }

    fn dispatch(&self, path: &EffectPath, branches: usize, concurrency: usize) {
        self.inner.dispatch(path, branches, concurrency);
    }

    fn register_tree_branches(&self, node_path: &EffectPath, branches: &[electricity_vm::NodeRef]) {
        self.inner.register_tree_branches(node_path, branches);
    }

    fn clear_tree_branches(&self, node_path: &EffectPath, branches: &[electricity_vm::NodeRef]) {
        self.inner.clear_tree_branches(node_path, branches);
    }

    fn record_totals(&self, meta: &Value) {
        self.inner.record_totals(meta);
    }
}

/// A bare-minimum [`RunContext`] -- an unconfigured registry/limiter,
/// `model: ""`, `adapter: "_noop"` (run-wiring step 15 of issue #431's
/// table), `dry_run: false`.
pub fn run_ctx<'a>(
    registry: &'a electricity_tools::ToolRegistry,
    limiter: &'a Limiter,
) -> RunContext<'a> {
    // Leaked: no test in this support module's own callers reads any
    // of these M1-A fields yet, so a `'static` default is simpler than
    // threading four more lifetimes through every one of this
    // function's own callers across the test suite.
    let adapters: &'a electricity_vm::adapter::AdapterRegistry =
        Box::leak(Box::new(electricity_vm::adapter::AdapterRegistry::new()));
    let complexity: &'a electricity_config::ComplexitySettings =
        Box::leak(Box::new(electricity_config::ComplexitySettings::default()));
    let dir: &'static std::path::PathBuf = Box::leak(Box::new(std::path::PathBuf::from(".")));
    let declared_prompts: &'a indexmap::IndexMap<String, String> =
        Box::leak(Box::new(indexmap::IndexMap::new()));
    let effect_names: &'a std::collections::BTreeSet<String> =
        Box::leak(Box::new(std::collections::BTreeSet::new()));
    RunContext {
        registry,
        limiter,
        model: "",
        adapter: "_noop",
        runtime_config: EMPTY_RUNTIME_CONFIG,
        dry_run: false,
        armed: false,
        adapters,
        default_adapter: std::rc::Rc::new(electricity_vm::adapter::NoopAdapter),
        model_locked: false,
        adapter_timeout_seconds: 120,
        complexity,
        decomposition_depth: 0,
        orchestration_dir: dir.as_path(),
        declared_prompts,
        effect_names,
        supported: electricity_bytecode::Supported::m0(&[]),
    }
}

pub static EMPTY_RUNTIME_CONFIG: &Value = &Value::None;
