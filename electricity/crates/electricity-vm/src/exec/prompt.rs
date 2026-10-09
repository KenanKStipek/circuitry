//! `prompt` effect execution (`core/prompt.py::PromptRuntime.execute`) --
//! signature only; lane G fills the body. Issue #449's gate lane, item
//! 8: routed from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::prompt`] is `true`.

use crate::exec::CtxChain;
use crate::{CancellationToken, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{Op, PromptOp};

/// Runs *prompt* (a `prompt` effect's own compiled fields) against
/// *store*, under *parent*, with *ctx_chain* the not-yet-materialized
/// rendering context (see [`crate::exec::CtxSource`]) its own
/// `inputs:`/adapter `params:` are read from **raw** (never rendered --
/// the IR snapshot test in `electricity-compiler`'s own test suite pins
/// that the compiler already keeps both as `ParamNode::Literal`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_prompt(
    op: &Op,
    _prompt: &PromptOp,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    Err(VmError::NotImplemented(format!(
        "{}: `prompt` is not supported by this build of electricity yet (M1-G)",
        op.path
    )))
}
