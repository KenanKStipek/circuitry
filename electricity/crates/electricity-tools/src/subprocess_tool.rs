//! `plugins/_subprocess.py::GenericSubprocessTool`, ported -- the one
//! pass-through [`crate::ToolPlugin`] shape `awk`/`imagemagick`/
//! `ffmpeg` (and every other binary-wrapping plugin Circuitry has)
//! share: forward `params.args` to a named binary, `params.cwd`/
//! `params.stdin`/`params.allow_nonzero` as the only other knobs.
//! `binary`/`env` come from `runtime.plugins.<name>` config
//! (`plugin_binary_override`/`plugin_env_override`), never `params` --
//! a machine-specific executable path or thread-limit variable belongs
//! in config, not in orchestration YAML. Signature only (M1 gate seam
//! round 2, item S9); lane C fills the body ([`crate::process::run_tracked`]
//! dispatch, `allow_nonzero`'s own `ok`/`exit_code` split).

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

/// One instance per binary-wrapping plugin file (`awk.rs`,
/// `imagemagick.rs`, `ffmpeg.rs`, and lane C's own later additions),
/// each supplying its own *name*/*binary_candidates* -- `plugins/
/// _subprocess.py::GenericSubprocessTool`'s own four fields.
pub struct GenericSubprocessTool {
    pub name: String,
    pub binary_candidates: Vec<String>,
    pub binary: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
}

#[async_trait(?Send)]
impl ToolPlugin for GenericSubprocessTool {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(format!(
            "{}: not implemented yet (lane C)",
            self.name
        )))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some(format!("{}: not implemented yet (lane C)", self.name)),
        }
    }
}

/// `plugins/_subprocess.py::plugin_binary_override` -- signature only;
/// lane C fills the body (`cfg.get("binary")`, `~` expansion deferred
/// to resolution time).
pub fn plugin_binary_override(_cfg: &Value) -> Option<String> {
    None
}

/// `plugins/_subprocess.py::plugin_env_override` -- signature only;
/// lane C fills the body (the mapping-shape validation and its exact
/// error text).
pub fn plugin_env_override(
    _cfg: &Value,
    _plugin_name: &str,
) -> Result<Option<std::collections::HashMap<String, String>>, ToolError> {
    Ok(None)
}
