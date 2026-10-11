//! `use` effect execution (`core/use.py::UseRuntime`) -- signature
//! only; lane I fills the body. Issue #449's gate lane, item 8: routed
//! from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::use_`] is `true`.
//! [`run_isolated`] is shared with M1-L2's decomposition lane (the same
//! "run a child document with its own store, grafted back under the
//! parent" primitive `core/use.py::_run_isolated` is).
//!
//! Review P2-7 on PR #454's second review: the child's own root
//! container (`core/use.py`'s own `_CHILD_ROOT`, `"prime"`) fires no
//! `effect_start`/`effect_complete` of its own when forwarded to the
//! parent's own observer (`core/use.py:43-56`'s own
//! `_relative_child_path`/`_namespaced_effect_cb`, `None` for the
//! child root specifically) -- the parent's own `use` effect node
//! already represents it, so forwarding a pair for it too would be a
//! duplicate. Every other child effect's own pair forwards normally,
//! relative to the child root. Lane I must filter the child root's
//! own pair out of whatever [`RunObserver`] it hands [`run_isolated`]
//! (or the child's own freshly-built store/observer plumbing,
//! whichever this lane's own design ends up threading it through),
//! not just relabel every path.

use crate::exec::CtxChain;
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Op, UseOp};

/// Runs a `use` effect: resolves *use_op*'s own `source` (a path, a
/// named orchestration, or inline YAML), compiles it, and runs it
/// isolated ([`run_isolated`], called with `frame.nested(&op.path)` --
/// the child program's own compiled paths start fresh at `prime`, with
/// no idea of the parent's own absolute position, exactly the case
/// [`Frame::nested`]'s own doc comment calls out) before grafting its
/// outputs back under *parent*.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_use(
    op: &Op,
    _use_op: &UseOp,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _frame: &Frame,
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
    _frame: &Frame,
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
