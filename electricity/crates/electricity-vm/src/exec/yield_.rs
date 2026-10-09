//! `yield` effect execution (the run-time half of #406) -- signature
//! only; lane P fills the body. Issue #449's gate lane, item 8: routed
//! from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::yield_`] is `true`.

use crate::exec::CtxChain;
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Op, YieldOp};

/// Runs a `yield` effect: renders *yield_op*'s own `template` (with
/// `{{> name}}` partial expansion, [`crate::compose::render_with_composition`])
/// against *ctx_chain* merged with its own raw `inputs:`, and writes
/// the result as this effect's own `value`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_yield(
    op: &Op,
    _yield_op: &YieldOp,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    Err(VmError::NotImplemented(format!(
        "{}: `yield` is not supported by this build of electricity yet (M1-P)",
        op.path
    )))
}
