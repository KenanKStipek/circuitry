//! Disabled effects (`core/disabled.py`) -- signatures only; lane J2
//! fills the bodies. Issue #449's gate lane, item 8.
//!
//! [`electricity_bytecode::Op::enabled`] already exists on the IR (M0-G);
//! the VM does not read it yet (#442's own "Disabled effects" item) --
//! neither function below has a real caller in this build, so a
//! document with `enabled: false` on any effect runs that effect
//! normally, exactly as every M0 test already relies on. Lane J2 wires
//! both in once profiles (M1-J1/J2) can actually produce one.

use crate::{NodeRef, Store, VmError};
use electricity_bytecode::Op;
use electricity_value::Value;

/// `core/disabled.py`'s own write shape: `{value: None, meta: {
/// disabled: true, created_at, completed_at}}`, with a balanced
/// `effect_start`/`effect_complete` pair around it (`on_write` fires
/// too, once wired).
pub fn write_disabled_node(_store: &Store, _parent: &NodeRef, op: &Op) -> Result<(), VmError> {
    Err(VmError::NotImplemented(format!(
        "{}: disabled-effect bookkeeping is not supported by this build of electricity yet (M1-J2)",
        op.path
    )))
}

/// Whether *op* (or an ancestor container) is disabled -- a disabled
/// container disables its whole subtree (`core/disabled.py::
/// _skip_disabled_effect`'s own container rule).
pub fn skip_disabled(op: &Op, _ctx: &Value) -> bool {
    !op.enabled
}
