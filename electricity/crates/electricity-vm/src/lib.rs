//! electricity-vm: the M0-H VM (issue #431's Scope section) -- `store`
//! (lane B), `exec::{dynamic, conditional}` (lane B) plus `exec::tool`
//! (lane A, real), `limiter`/`cancel` (lane B), `observer` (lane A,
//! real). [`execute_root`] is lane A's one remaining stub: the actual
//! tree-walking interpreter loop lane B builds.
//!
//! See `electricity/docs/spec/vm-lanes.md` for which lane owns which
//! file and function in this crate (and its sibling VM-lane crates).

pub mod cancel;
pub mod exec;
pub mod limiter;
pub mod observer;
pub mod store;

pub use cancel::CancellationToken;
pub use limiter::{Limiter, LimiterError, SlotGuard};
pub use observer::{NullObserver, RunObserver};
pub use store::{NodeRef, Store, StoreError};

use electricity_bytecode::Program;
use electricity_value::Value;
use std::fmt;

/// Any error [`execute_root`] (or one of its own sub-executors, once
/// lane B builds them) can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    /// A `tool` effect's `provider:` has no registered
    /// [`electricity_tools::ToolPlugin`].
    ToolNotFound(String),
    /// A registered plugin's own [`electricity_tools::ToolError`], as text.
    Tool(String),
    /// Lane B's own execution loop isn't implemented yet.
    NotImplemented(String),
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmError::ToolNotFound(provider) => write!(f, "Unknown tool provider: {provider}"),
            VmError::Tool(message) => write!(f, "{message}"),
            VmError::NotImplemented(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for VmError {}

/// Runs *program*'s root against *store*, with *ctx* as the template/CEL
/// rendering context (the materialized state snapshot `{from: ...}`/
/// `{{ }}`/CEL expressions read from -- DESIGN.md's own `scope_ctx`),
/// reporting every effect start/complete/dispatch/write to *observer*,
/// and checking *token* before each chain effect / before starting each
/// queued tree branch (DESIGN §6.5/§6.9).
///
/// Lane A stub: always `Err(VmError::NotImplemented(..))` -- lane B's own
/// `exec::dynamic`/`exec::conditional` tree-walking interpreter.
pub async fn execute_root(
    program: &Program,
    store: &Store,
    ctx: &Value,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let _ = (program, store, ctx, observer, token);
    Err(VmError::NotImplemented(
        "electricity_vm::execute_root is not implemented yet (lane B, issue #431)".to_string(),
    ))
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
    async fn execute_root_is_a_lane_b_stub() {
        let program = empty_program();
        let store = Store::new();
        let token = CancellationToken::new();
        let err = execute_root(&program, &store, &Value::None, &NullObserver, &token)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::NotImplemented(_)));
    }
}
