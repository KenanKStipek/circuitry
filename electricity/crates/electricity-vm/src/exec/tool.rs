//! `run_tool`: the VM's own tool-dispatch entry point -- looks a `tool`
//! effect's `provider:` up in an [`electricity_tools::ToolRegistry`] and
//! runs it. The dispatch seam itself is final (lane B/C never edit this
//! file to wire a new provider in; they only build the registry they
//! pass to it); *provider* normalization already matches `plugins/
//! factory.py::build_plugin`'s own `.strip().lower()` (below), but the
//! exact "unknown provider" wording still diverges -- this crate's own
//! [`crate::VmError::ToolNotFound`] text isn't `build_plugin`'s own
//! `"Unknown plugin: 'x'. Supported plugins: ..."` -- so lane C may
//! still widen that text with the supported-provider list once it has
//! the full `electricity_tools::ToolRegistry` populated to word that
//! list from.
//!
//! [`execute_tool`] is a separate, higher-level entry point, also owned
//! by this file -- lane C's own stub (`run_tool` above stays lane A's).
//! See its own doc comment for what it owns.

use crate::{CancellationToken, RunContext, RunObserver, VmError};
use electricity_bytecode::{Op, ToolOp};
use electricity_tools::ToolRegistry;
use electricity_value::Value;

/// Runs *provider* (a tool effect's own `provider:`) against *registry*,
/// with *params* already rendered (lane B/C's own `params.rs` -- `{from:
/// ...}` resolved, templates rendered, `params_json` deep-merged; out of
/// scope for this function) and *timeout_seconds* already resolved by
/// the caller (`core/tool.py::ToolRuntime._resolve_timeout_seconds`'s
/// own `timeout_ms`-vs-`runtime.tools.timeout_seconds`-vs-default
/// policy needs the merged runtime config this function doesn't take --
/// [`crate::RunContext::runtime_config`] is where lane B's own caller
/// reads it from before calling this).
///
/// `Err` is [`VmError::ToolNotFound`] for an unregistered *provider*, or
/// the plugin's own [`electricity_tools::ToolError`] wrapped the same
/// way.
pub async fn run_tool(
    registry: &ToolRegistry,
    provider: &str,
    params: Value,
    timeout_seconds: u32,
) -> Result<electricity_tools::ToolResult, VmError> {
    let normalized = provider.trim().to_lowercase();
    let plugin = registry
        .get(&normalized)
        .ok_or_else(|| VmError::ToolNotFound(provider.to_string()))?;
    plugin
        .execute(params, timeout_seconds)
        .await
        .map_err(|err| VmError::Tool(err.to_string()))
}

/// A single `tool` effect's own owning entry point -- lane C's stub
/// (not `run_tool` above, lane A's own final dispatch seam that this
/// function calls once it has something rendered to dispatch).
/// `exec::dynamic`/`exec::conditional` (lane B, not yet written) call
/// this once per `tool` node they execute; it owns everything `core/
/// tool.py::ToolRuntime.run` does that `run_tool` itself doesn't:
///
/// - the `meta` reset at the start of each attempt (`core/tool.py`'s
///   own clearing of a previous attempt's `error`/`raw` before trying
///   again);
/// - rendering *tool*'s `params`/`params_json` against *ctx* (`{from:
///   ...}` resolved, Mustache leaves rendered, `params_json` deep-merged
///   -- the lane-C-owned `params.rs` this crate doesn't have yet);
/// - `retries`/backoff between attempts, and a CEL `expect:` evaluated
///   against each attempt's own result;
/// - acquiring a [`crate::Limiter`] slot (*op*'s own `group:`, if any)
///   for *each* attempt, released before that attempt's own backoff
///   sleep, not held across it;
/// - running the rendered params through [`run_tool`], then redacting
///   and 64 KiB-capping the result before it's writable at all (this
///   crate's sibling `electricity-redaction`);
/// - writing the outcome into *store* at *parent* (`meta.raw`/`meta.
///   error`/the tool's own declared output keys) and reporting it to
///   *observer* (`effect_start`/`effect_complete`/`write`);
/// - checking *token* between attempts (DESIGN §6.5/§6.9: a cancelled
///   run doesn't start a fresh retry, though an attempt already in
///   flight still finishes it, same as any other in-flight effect).
///
/// *op* is the `tool` node's own [`electricity_bytecode::Op`] (its path,
/// `on_error`, name); *tool* is that node's [`ToolOp`] payload. *run_ctx*
/// is where this function reads the registry, the limiter, and the
/// merged runtime config (tool timeout/allowlist resolution) from --
/// see [`crate::RunContext`]'s own doc comment.
///
/// Lane A stub: always `Err(VmError::NotImplemented(..))` -- every
/// parameter already final; lane C replaces the body without touching
/// this signature.
// Eight parameters, matching `execute_root`'s own split of "the node
// and its payload", "where state lives", "the render/run context" and
// "how to report and cancel" -- a struct bundling some of them would
// only move the coupling, not remove it, and lane C may need to peel a
// couple back out again once it has a real body to write.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool(
    op: &Op,
    tool: &ToolOp,
    store: &crate::Store,
    parent: &crate::NodeRef,
    ctx: &Value,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let _ = (op, tool, store, parent, ctx, run_ctx, observer, token);
    Err(VmError::NotImplemented(
        "electricity_vm::exec::tool::execute_tool is not implemented yet (lane C, issue #431)"
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unregistered_provider_is_reported_by_name() {
        let registry = ToolRegistry::new();
        let err = run_tool(&registry, "json", Value::None, 300)
            .await
            .unwrap_err();
        assert_eq!(err, VmError::ToolNotFound("json".to_string()));
    }

    #[tokio::test]
    async fn a_provider_name_is_normalized_before_lookup() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "  JSON ", Value::None, 300)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }

    #[tokio::test]
    async fn a_registered_providers_own_error_is_wrapped() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let err = run_tool(&registry, "json", Value::None, 300)
            .await
            .unwrap_err();
        assert!(matches!(err, VmError::Tool(_)));
    }

    fn tool_op() -> ToolOp {
        ToolOp {
            provider: "json".to_string(),
            params: electricity_bytecode::ParamNode::Map(indexmap::IndexMap::new()),
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: electricity_bytecode::RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }
    }

    fn tool_node() -> Op {
        Op {
            path: electricity_bytecode::EffectPath::root().push_name("fetch"),
            name: Some("fetch".to_string()),
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Tool(tool_op()),
            )),
            on_error: electricity_bytecode::OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn execute_tool_is_a_lane_c_stub() {
        let op = tool_node();
        let tool = tool_op();
        let store = crate::Store::new();
        let registry = ToolRegistry::new();
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: false,
        };
        let token = CancellationToken::new();
        let err = execute_tool(
            &op,
            &tool,
            &store,
            &store.root,
            &Value::None,
            &run_ctx,
            &crate::NullObserver,
            &token,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, VmError::NotImplemented(_)));
    }
}
