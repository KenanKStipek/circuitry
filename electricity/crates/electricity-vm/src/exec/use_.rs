//! `use` effect execution (`core/use.py::UseRuntime`) -- signature
//! only; lane I fills the body. Issue #449's gate lane, item 8: routed
//! from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::use_`] is `true`.
//! [`run_isolated`] is shared with M1-L2's decomposition lane (the same
//! "run a child document with its own store, grafted back under the
//! parent" primitive `core/use.py::_run_isolated` is).

use crate::exec::CtxChain;
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Op, UseOp};

/// Runs a `use` effect: resolves *use_op*'s own `source` (a path, a
/// named orchestration, or inline YAML), compiles it, and runs it
/// isolated ([`run_isolated`]) before grafting its outputs back under
/// *parent*.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_use(
    op: &Op,
    _use_op: &UseOp,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    Err(VmError::NotImplemented(format!(
        "{}: `use` is not supported by this build of electricity yet (M1-I)",
        op.path
    )))
}

/// `core/use.py::_run_isolated` -- runs *program* against a fresh,
/// isolated [`Store`] (never the parent's own), namespacing its own
/// events/scripted-adapter keys under *namespace_path*. Shared by
/// `use` (lane I) and decomposition (lane L2).
pub async fn run_isolated(
    program: &electricity_bytecode::Program,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
    namespace_path: &str,
) -> Result<Store, VmError> {
    Err(VmError::NotImplemented(format!(
        "{namespace_path}: isolated sub-document execution is not supported by this build of \
         electricity yet (M1-I/M1-L2, document {:?})",
        program.document.as_ref().map(|d| &d.path_as_given)
    )))
}
