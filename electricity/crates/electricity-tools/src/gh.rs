//! `plugins/gh.py`, ported -- a pass-through to the `gh` binary (issue
//! #722). Args via `params.args`; authentication is whatever `gh auth
//! login` already set up outside circuitry, same as `cof run`.

use crate::subprocess_tool::GenericSubprocessTool;
use crate::{ToolError, ToolPlugin};
use electricity_value::Value;

/// `plugins/gh.py::make_plugin`.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    Ok(Box::new(GenericSubprocessTool {
        name: "gh".to_string(),
        binary_candidates: vec!["gh".to_string()],
        binary: crate::subprocess_tool::plugin_binary_override(cfg),
        env: crate::subprocess_tool::plugin_env_override(cfg, "gh")?,
    }))
}
