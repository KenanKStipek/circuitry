//! `loop` effect execution (`core/loop.py::LoopRuntime.execute`) --
//! signature only; lane H fills the body. Issue #449's gate lane, item
//! 8: routed from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::loop_`] is `true`.

use crate::exec::CtxChain;
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{LoopFlow, LoopSpec, Op, Region};
use electricity_value::Value;

/// Runs a `loop` effect's own `each:`/`while:` body, chain or tree, as
/// many passes as *spec*/*max_iterations*/*min_iterations* allow.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_loop(
    op: &Op,
    _spec: &LoopSpec,
    _body: &Region,
    _flow: LoopFlow,
    _max_concurrency: Option<u32>,
    _max_iterations: Option<u32>,
    _min_iterations: u32,
    _collect: Option<&str>,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    Err(VmError::NotImplemented(format!(
        "{}: `loop` is not supported by this build of electricity yet (M1-H)",
        op.path
    )))
}

/// `core/loop.py::_loop_progress` -- `meta.progress`'s own
/// `{done, total, elapsed_s, eta_s}` shape, recomputed after every
/// pass. Pass-through stub: no caller in this build ever records a
/// pass yet, so there is nothing real to report.
pub fn loop_progress(_done: u32, _total: Option<u32>, _elapsed_s: f64) -> Value {
    Value::None
}
