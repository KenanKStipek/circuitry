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

pub mod cancel;
pub mod exec;
pub mod limiter;
pub mod observer;
pub mod params;
pub mod store;

pub use cancel::CancellationToken;
pub use limiter::{Limiter, LimiterError, SlotGuard};
pub use observer::{NullObserver, RunObserver};
pub use store::{NodeRef, Slot, Store, StoreError};

use electricity_bytecode::Program;
use electricity_tools::ToolRegistry;
use electricity_value::Value;
use std::fmt;

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
    pub adapter: &'a str,
    pub runtime_config: &'a Value,
    /// `--dry-run`-style execution with no tool side effects -- reserved
    /// for a later milestone; always `false` through M0-H's own run
    /// wiring (issue #431 never mentions a dry-run flag), kept here
    /// rather than added to this struct's shape later.
    pub dry_run: bool,
}

/// Any error [`execute_root`] (or one of its own sub-executors) can
/// return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    /// A `tool` effect's `provider:` has no registered
    /// [`electricity_tools::ToolPlugin`].
    ToolNotFound(String),
    /// A registered plugin's own [`electricity_tools::ToolError`], or any
    /// other tool-effect failure (a render error, an allowlist denial, a
    /// failed `expect:`), as text -- what [`crate::exec::tool::execute_tool`]
    /// returns for `on_error: fail` once retries are exhausted.
    Tool(String),
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

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmError::ToolNotFound(provider) => write!(f, "Unknown tool provider: {provider}"),
            VmError::Tool(message) => write!(f, "{message}"),
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
    exec::dynamic::execute_dynamic(
        &program.root,
        store,
        &store.root,
        &chain,
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
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: false,
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
