//! `use` effect execution (`core/use.py::UseRuntime`) -- signature
//! only; lane I fills the body. Issue #449's gate lane, item 8: routed
//! from [`crate::exec::execute_op`] once
//! [`electricity_bytecode::refusal::Supported::use_`] is `true`.
//! [`run_isolated`] is shared with M1-L2's decomposition lane -- the
//! same "run a child document with its own store, grafted back under
//! the parent" primitive, though `core/use.py` has no `_run_isolated`
//! function of its own to port this from (M1 lane I prerequisites,
//! issue #448, item U5: an earlier revision of this doc comment
//! claimed one): the logic is inline in `UseRuntime.execute`
//! (`core/use.py:960-968` builds the child `Store` directly, no
//! helper call).
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

/// Test-only sentinel for [`test_only_refused`] -- never a real `use`
/// effect's own `path:` (the compiler rejects a non-existent file
/// outright were this ever reached through a real document; no
/// document in this workspace's own test corpus names a file this way
/// either). Not `#[cfg(test)]`: an integration test under `tests/`
/// links this crate built in its ordinary, non-test configuration (only
/// the crate's own unit-test binary ever sets `cfg(test)`), so a
/// sentinel gated that way would be invisible to exactly the chain/
/// `on_error`/`finally:` test this exists for.
const TEST_ONLY_REFUSED_SENTINEL: &str = "__test_only_refused__";

/// A canned [`VmError::Refused`] for *use_op* when its own `source` is
/// [`TEST_ONLY_REFUSED_SENTINEL`] -- the one way this crate's own test
/// suite can exercise [`VmError::Refused`]'s propagation (unwrapped
/// through `wrap_effect_error`, never absorbed by `on_error`, `finally:`
/// still running) through a real, compiled `Op` tree and [`crate::
/// execute_root`], before lane I's real `execute_use` body exists to
/// produce one for real (M1 lane I prerequisites, issue #448, item U3).
/// Lane I deletes this -- its own real body decides when a freshly
/// compiled child is refused, the same way the top-level document
/// already was before the run started. Inert for any real `use`
/// effect: no document in this workspace's own test corpus names a
/// `path:` this way, and `use` is refused before any real run reaches
/// this function at all while `Supported::use_` stays `false`.
fn test_only_refused(use_op: &UseOp) -> Option<VmError> {
    let UseOp {
        source: electricity_bytecode::UseSource::Path(path),
        ..
    } = use_op
    else {
        return None;
    };
    if path != TEST_ONLY_REFUSED_SENTINEL {
        return None;
    }
    Some(VmError::Refused {
        refusal: electricity_bytecode::Refusal {
            path: electricity_bytecode::EffectPath::root().push_name("step"),
            reason: electricity_bytecode::RefusalReason::Loop,
        },
        child_label: path.clone(),
    })
}

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
    use_op: &UseOp,
    _store: &Store,
    _parent: &NodeRef,
    _ctx_chain: &CtxChain,
    _frame: &Frame,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<(), VmError> {
    if let Some(refused) = test_only_refused(use_op) {
        return Err(refused);
    }
    Err(VmError::NotImplemented(format!(
        "{}: `use` is not supported by this build of electricity yet (M1-I)",
        op.path
    )))
}

/// Runs *program* against a fresh, isolated [`Store`] seeded with
/// *initial_state* (never the parent's own store) -- there is no
/// `core/use.py::_run_isolated` to port this from: `UseRuntime.execute`
/// builds its own child `Store` inline (`core/use.py:960-968`), with no
/// separate function of its own. *initial_state* is that same shape,
/// `{"input": <rendered+checked inputs>}` for a `use` effect -- the
/// caller seeds it (lane I renders/checks `use.inputs` first, exactly
/// as `core/use.py::UseRuntime.execute` does, then builds this value),
/// not this function, since a decomposition child (lane L2) may seed
/// something else entirely. No `namespace_path` parameter: it
/// duplicated `frame.nested`'s own prefix (M1 lane I prerequisites,
/// issue #448, item U5) -- *frame* already carries the child's own
/// absolute path in the parent document (`Frame::nested`'s own doc
/// comment), the one thing a `namespace_path: &str` of this function's
/// own could ever have named. Shared by `use` (lane I) and
/// decomposition (lane L2).
pub async fn run_isolated(
    program: &electricity_bytecode::Program,
    _initial_state: electricity_value::Value,
    _frame: &Frame,
    _run_ctx: &RunContext<'_>,
    _observer: &dyn RunObserver,
    _token: &CancellationToken,
) -> Result<Store, VmError> {
    Err(VmError::NotImplemented(format!(
        "isolated sub-document execution is not supported by this build of electricity yet \
         (M1-I/M1-L2, document {:?})",
        program.document.as_ref().map(|d| &d.path_as_given)
    )))
}
