//! `reflector` effect execution (`core/reflector.py`) -- signature
//! only; lane L1 fills the body. Issue #449's gate lane, item 8: routed
//! from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::reflector`] is `true`
//! (and, once it is, [`electricity_bytecode::refusal::first_unsupported`]
//! still recurses into [`electricity_bytecode::ReflectorOp::inner`] to
//! find an unsupported effect nested inside it -- this function is the
//! one that actually has to run that nested region, in a loop, up to
//! `max_iterations`/`max_effects`).

use crate::exec::CtxChain;
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Op, ReflectorOp};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_reflector(
    op: &Op,
    _reflector: &ReflectorOp,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    Err(VmError::NotImplemented(format!(
        "{}: `reflector` is not supported by this build of electricity yet (M1-L1)",
        op.path
    )))
}
