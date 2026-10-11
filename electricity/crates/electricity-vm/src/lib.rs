//! electricity-vm: the M0-H VM (issue #431's Scope section) -- `store`,
//! `exec::{mod,dynamic,conditional}` and `limiter` (lane B); `exec::
//! tool::execute_tool` and `electricity-tools`'s `json` (lane C, params
//! rendering/retries/redaction); `exec::tool::run_tool` and `cancel`
//! (lane A, real, final). [`execute_root`] is lane B2's tree-walking
//! interpreter loop: a document root is always a `dynamic`-shaped
//! container, so running it is exactly `exec::dynamic::execute_dynamic`
//! against the store's own root node.
//!
//! `execute_root` takes every argument by reference, so lane B's own
//! tree branches can't each be a `tokio::task::spawn_local` (which
//! needs `'static`, DESIGN.md §6.1-6.2's own phrasing notwithstanding)
//! -- running them as a set of futures inside one `FuturesUnordered`,
//! gated by [`Limiter`] for `max_concurrency`, polled from the same
//! single `LocalSet` task, gets the same cooperative-concurrency
//! behavior without the `'static` bound.
//!
//! See `electricity/docs/spec/vm-lanes.md` for which lane owns which
//! file and function in this crate (and its sibling VM-lane crates).

pub mod adapter;
pub mod adapter_names;
pub mod compose;
pub mod exec;
pub mod frame;
pub mod limiter;
pub mod observer;
pub mod params;
pub mod retry;
pub mod store;

/// Moved to `electricity-value` (issue #449's gate lane): a `ToolCall`
/// (`electricity-tools`) and the VM both need the same concrete token
/// type, and the workspace's dependency direction (DESIGN.md §2) rules
/// out defining it in either one -- re-exported here unchanged so every
/// existing `electricity_vm::CancellationToken` path keeps working.
pub use electricity_value::CancellationToken;
pub use limiter::{Limiter, LimiterError, SlotGuard};
pub use observer::{NullObserver, RunObserver};
pub use store::{NodeRef, Slot, Store, StoreError};

use electricity_bytecode::Program;
use electricity_config::ComplexitySettings;
use electricity_tools::ToolRegistry;
use electricity_value::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

pub use adapter::Adapter;
pub use frame::Frame;

/// Everything [`execute_root`]'s own tree-walking interpreter (lane B)
/// and [`exec::tool::run_tool`] (through it) need that isn't the store,
/// the template/CEL context, the observer, or the cancellation token --
/// the tool registry a `tool` effect dispatches through, the run-wide
/// concurrency limiter a `tool`/`dynamic` effect's `group:` acquires a
/// slot from, the two run-level settings every `meta` entry records
/// (`model`; `adapter`, always `"_noop"` in M0-H -- issue #431's Scope
/// section, run-wiring step 15), and the merged `runtime:` block
/// `run_tool`'s own timeout/allowlist resolution (lane C/D) and
/// `ToolRuntime` both read off it (run-wiring step 8's own
/// `runtime_config`).
///
/// Lane A stub: a plain bag of references with no behaviour of its own.
/// Lane D's own `run_orchestration` builds one from its
/// `electricity_config::EffectiveSettings` and a populated
/// [`electricity_tools::ToolRegistry`]/[`Limiter`], once both exist;
/// lane B's `execute_root` body is the first real reader. Added so this
/// crate's own [`execute_root`] signature -- `vm-lanes.md`'s own "should
/// not need to change" row -- doesn't force lane B or lane D to add a
/// parameter to a function `electricity-cli`/`electricity` (lib) already
/// call by the time either lane starts.
pub struct RunContext<'a> {
    pub registry: &'a ToolRegistry,
    pub limiter: &'a Limiter,
    pub model: &'a str,
    /// M0-H's own bare adapter-name field, kept verbatim (always
    /// `"_noop"` through M1-A: run-wiring step 15) -- a `dynamic` node's
    /// own `meta.adapter` reads this directly, never [`Self::default_adapter`]
    /// (`exec::dynamic::write_dynamic_meta_start`).
    pub adapter: &'a str,
    pub runtime_config: &'a Value,
    /// `--dry-run`-style execution with no tool side effects -- reserved
    /// for a later milestone; always `false` through M0-H's own run
    /// wiring (issue #431 never mentions a dry-run flag), kept here
    /// rather than added to this struct's shape later.
    pub dry_run: bool,
    /// `true` only for a CLI-driven run -- [`electricity_tools::ToolCall::armed`]'s
    /// own doc comment (issue #449's gate lane item 1).
    pub armed: bool,
    /// `adapters/factory.py::build_adapter`'s own per-attempt builder
    /// table (issue #449's gate lane item 4) -- lane G's `prompt` effect
    /// is the first real reader.
    pub adapters: &'a adapter::AdapterRegistry,
    /// The run's own default adapter, already built -- `adapter::NoopAdapter`
    /// for every M0/M1-A run (no real adapter is ever configured yet).
    /// `Rc`, not `Box`/a bare reference: a fallback attempt chain
    /// (lane G) hands this same instance to more than one concurrent
    /// borrow site without this struct itself needing to be `Clone`.
    pub default_adapter: Rc<dyn adapter::Adapter>,
    /// A profile's run-level `model:` locks routing off for every prompt
    /// in the run (`model_reason` stays `"default"`) -- lane J2/K2.
    pub model_locked: bool,
    /// `adapters/factory.py::configured_timeout_seconds(...) or 120` --
    /// the run's own adapter dispatch timeout, before a per-prompt
    /// `timeout_ms` can shorten it (lane G).
    pub adapter_timeout_seconds: u32,
    /// The resolved `runtime.complexity` settings (`electricity_config::
    /// complexity::validate_complexity`) -- lane K1/K2.
    pub complexity: &'a ComplexitySettings,
    /// How many `use`/decomposition levels deep this run already is --
    /// lane I/L's own recursion-depth ceiling. `core/use.py`'s own
    /// `use_call_stack` cycle-detection stack is deliberately *not* a
    /// field here: an earlier revision of this struct carried one as a
    /// `&'a RefCell<Vec<String>>`, shared and mutated in place, which
    /// is exactly the false-cycle hazard `core/use.py:856-871` warns
    /// about for concurrent tree/loop branches (M1 gate seam round 2,
    /// item S6) -- a shared mutable stack can't tell two sibling
    /// branches apart, so one branch's own push is visible to the
    /// other as a false ancestor. [`crate::Frame::push_use`]'s own
    /// per-call, `Rc`-shared-but-never-mutated-in-place stack (built
    /// fresh per branch, the same way Python derives *parent_stack* as
    /// a list copy rather than mutating `runtime_config["_use_call_stack"]`
    /// itself) is the only one; lane I reads it via
    /// [`crate::Frame::use_call_stack`] off the `Frame` already threaded
    /// to every container executor, never off this struct.
    pub decomposition_depth: u32,
    /// The directory a relative `use: {path: ...}`/asset reference
    /// resolves against -- `Program.document`'s own `resolved_directory`.
    /// Lane I.
    pub orchestration_dir: &'a Path,
    /// `Program.prompts` -- the document's own top-level `prompts:` map
    /// (name -> template text), for `{{> name}}` expansion. Lane P.
    pub declared_prompts: &'a indexmap::IndexMap<String, String>,
    /// `Program.effect_names` -- every named effect in the document, for
    /// `{{> name}}`'s own "not a declared prompt" vs "a declared prompt
    /// that hasn't run yet" distinction. Lane P.
    pub effect_names: &'a BTreeSet<String>,
    /// `core/prompt.py`'s own container-nesting display depth
    /// (`structure.depth`), threaded alongside the store path rather
    /// than derived from it -- see electricity/docs/spec/vm-lanes.md's
    /// M1 table, §4 finding 3. Lane K1.
    pub display_depth: u32,
    /// This run's own capability set -- `electricity` (lib)'s run wiring
    /// builds it once, from `electricity_bytecode::refusal::Supported::
    /// current(...)` with the `test-tools` cargo feature's own extras,
    /// the same value already passed to `first_unsupported` for the
    /// top-level document. Lane I's own runtime check (M1 table:
    /// "first_unsupported + allowlist on each loaded child program")
    /// reads it off here rather than rebuilding it, so a freshly
    /// compiled `use` child is refused against the exact same
    /// capability set the parent document was checked against before
    /// the run started.
    pub supported: electricity_bytecode::Supported<'a>,
}

/// Any error [`execute_root`] (or one of its own sub-executors) can
/// return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    /// A `tool` effect's `provider:` has no registered
    /// [`electricity_tools::ToolPlugin`] -- `electricity_tools::registry::
    /// build_plugin`'s own exact unknown-plugin text (issue #449's gate
    /// lane item 2), or a registered plugin's own
    /// [`electricity_tools::ToolError`], or any other tool-effect failure
    /// (a render error, an allowlist denial, a failed `expect:`) widened
    /// to the same shape with [`electricity_tools::ToolError::message`]'s
    /// own `RuntimeError`/[`electricity_tools::ErrorCause::None`]
    /// default -- what [`crate::exec::tool::execute_tool`] returns for
    /// `on_error: fail` once retries are exhausted (review finding 1 on
    /// PR #454: the previous `Tool(String)` shape threw its own
    /// [`electricity_tools::ToolError`]'s *py_class*/*cause* away at
    /// [`crate::exec::tool::run_tool`], which #442's tree text and lane
    /// E's own HTTP-family retry rule both need). [`crate::exec::
    /// wrap_effect_error`] keeps *cause* unchanged through a chain/tree
    /// wrap but, review P2-1 on PR #454's second review, rewrites
    /// *py_class* to `RuntimeError` on every wrap (`core/dynamic.py`'s
    /// own `raise RuntimeError(...) from e`: the wrapping exception is
    /// always a `RuntimeError`, never *e*'s own class) and sets
    /// *cause_class* to the wrapped error's own previous *py_class* --
    /// never further than one level in, so a doubly-wrapped error's own
    /// *cause_class* is `RuntimeError` (the inner wrap's class), not the
    /// original leaf failure's class three levels down. [`VmError::
    /// displayed_class`] is `err.__cause__ or err`'s own one-level-only
    /// unwrap (`dynamic.py:56-57`), the class #442's own `[i] {type}`
    /// text prints.
    Raised {
        py_class: electricity_tools::PyExc,
        message: String,
        cause: electricity_tools::ErrorCause,
        /// `None` for a never-wrapped leaf failure (so
        /// [`VmError::displayed_class`] falls back to *py_class*
        /// itself, matching Python's own `err.__cause__ or err` when
        /// `__cause__` is `None`); `Some(previous py_class)` once a
        /// chain/tree container has wrapped it at least once.
        cause_class: Option<electricity_tools::PyExc>,
    },
    /// This run's own [`CancellationToken`] was requested while an effect
    /// was blocked waiting (a concurrency slot, a retry backoff) -- bypasses
    /// `on_error` entirely and fires no observer `effect_complete`, mirroring
    /// Python's `RunCancelledBySignal`: a `BaseException`, not `Exception`,
    /// so `ToolRuntime.execute`'s own `except Exception` never catches it
    /// (`core/cancellation.py`). The caller (lane B's own tree-walking
    /// interpreter) checks `token.is_set()`/`token.signum()` itself to build
    /// the interrupt text a cancelled run's `RunResult.error` carries; this
    /// variant only ever signals *that* cancellation happened here, not with
    /// what signal.
    Cancelled,
    /// Lane B's own execution loop isn't implemented yet (a `loop`/
    /// `mode: model` `if` -- refused before the run starts by lane A's
    /// `first_unsupported` in a real document, but the interpreter still
    /// reports this rather than panicking if one ever reaches it).
    NotImplemented(String),
    /// A plain error message -- a chain/tree child's own failure already
    /// wrapped with its effect path (`"<path>: <message>"`,
    /// `core/dynamic.py`'s own `RuntimeError(f"{effect_path}: {e}")`), a
    /// `TreeExecutionError`-shaped multi-branch summary, a conditional
    /// branch's own bare-name wrap, or a failed CEL evaluation's text.
    Message(String),
}

impl VmError {
    /// A plain message, defaulting to `RuntimeError`/
    /// [`electricity_tools::ErrorCause::None`] -- every failure this
    /// crate builds from its own text (a render error, an allowlist
    /// denial, a failed `expect:`) rather than from an actual
    /// [`electricity_tools::ToolError`] it already has both fields for.
    pub fn tool(message: impl Into<String>) -> VmError {
        VmError::Raised {
            py_class: electricity_tools::PyExc::RuntimeError,
            message: message.into(),
            cause: electricity_tools::ErrorCause::None,
            cause_class: None,
        }
    }

    /// `err.__cause__ or err`'s own class, one level only
    /// (`dynamic.py:56-57`) -- `None` for every [`VmError`] variant but
    /// [`VmError::Raised`], which has no Python exception class of its
    /// own to report.
    pub fn displayed_class(&self) -> Option<electricity_tools::PyExc> {
        match self {
            VmError::Raised {
                py_class,
                cause_class,
                ..
            } => Some(cause_class.unwrap_or(*py_class)),
            _ => None,
        }
    }
}

impl From<electricity_tools::ToolError> for VmError {
    fn from(err: electricity_tools::ToolError) -> VmError {
        VmError::Raised {
            py_class: err.py_class,
            message: err.message,
            cause: err.cause,
            cause_class: None,
        }
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmError::Raised { message, .. } => write!(f, "{message}"),
            VmError::Cancelled => write!(f, "cancelled"),
            VmError::NotImplemented(message) => write!(f, "{message}"),
            VmError::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for VmError {}

/// Runs *program*'s root against *store*, with *ctx* as the template/CEL
/// rendering context (the materialized state snapshot `{from: ...}`/
/// `{{ }}`/CEL expressions read from -- DESIGN.md's own `scope_ctx`),
/// with *run_ctx* for everything else the run needs that isn't *store*,
/// *ctx*, *observer* or *token* -- the tool registry, the limiter, the
/// model/adapter, and the merged runtime config (see [`RunContext`]'s
/// own doc comment) -- reporting every effect start/complete/dispatch/
/// write to *observer*, and checking *token* before each chain effect /
/// before starting each queued tree branch (DESIGN §6.5/§6.9).
///
/// *program*'s root is always a `dynamic`-shaped container (named
/// `prime`, DESIGN.md's own "document root as a non-overlay `dynamic`"
/// convention, issue #431's Scope section) -- so running it is exactly
/// [`exec::dynamic::execute_dynamic`] against *store*'s own root node,
/// with no `ctx` override of its own (*ctx* is Quirk Q1's `ctx_override`
/// parameter here, not a pre-resolved rendering context -- see that
/// function's own doc comment). [`exec::dynamic::execute_dynamic`] itself
/// takes that override as an [`exec::CtxChain`], not yet materialized
/// (so a reference several `dynamic` levels deep can still re-snapshot
/// every ancestor fresh instead of reading a frozen `Value`) -- *ctx*
/// here is always [`Value::None`] (the document root has no enclosing
/// scope of its own), which becomes the empty chain; a caller that ever
/// passed an already-resolved override `Value` gets it wrapped as the
/// chain's one frozen entry instead, so this function's own signature
/// can stay exactly what lane A fixed it as.
pub async fn execute_root(
    program: &Program,
    store: &Store,
    ctx: &Value,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let chain: exec::CtxChain = match ctx {
        Value::None => Vec::new(),
        other => vec![exec::CtxSource::Frozen(other.clone())],
    };
    let frame = Frame::root();
    exec::dynamic::execute_dynamic(
        &program.root,
        store,
        &store.root,
        &chain,
        &frame,
        run_ctx,
        observer,
        token,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::{DocumentInfo, NodeKind, OnError, Op, Region};
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn empty_program() -> Program {
        Program {
            root: Op {
                path: electricity_bytecode::EffectPath::root(),
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: Vec::new(),
                    overlay: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            },
            prompts: indexmap::IndexMap::new(),
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

    #[tokio::test]
    async fn execute_root_runs_an_empty_document_to_completion() {
        // No children at all (an empty chain `prime`) -- no `tool`/`if`
        // for lane C's still-stubbed `execute_tool` to ever reach, so
        // this exercises `execute_root`'s own wiring into
        // `exec::dynamic::execute_dynamic` without depending on lane C.
        let program = empty_program();
        let store = Store::new();
        let token = CancellationToken::new();
        let registry = ToolRegistry::new();
        let limiter = Limiter::new();
        let runtime_config = Value::None;
        let adapters = adapter::AdapterRegistry::new();
        let complexity = ComplexitySettings::default();
        let orchestration_dir = Path::new(".");
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir,
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            display_depth: 0,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        execute_root(
            &program,
            &store,
            &Value::None,
            &run_ctx,
            &NullObserver,
            &token,
        )
        .await
        .unwrap();
        let snapshot = store.snapshot(&store.root);
        let Value::Dict(root) = &snapshot else {
            panic!("expected a dict");
        };
        let Some(Value::Dict(prime)) = root.get(&Value::Str("prime".to_string())) else {
            panic!("expected a prime node");
        };
        assert_eq!(
            prime.get(&Value::Str("value".to_string())),
            Some(&Value::Bool(true))
        );
    }
}
