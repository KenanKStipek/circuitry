//! `plugins/awk.py`, ported -- signature only (M1 gate seam round 2,
//! item S9); lane C fills the body (`make_plugin`'s own binary
//! candidates, `("awk", "gawk", "mawk")`).

use crate::ToolPlugin;
use crate::subprocess_tool::GenericSubprocessTool;
use electricity_value::Value;

/// `plugins/awk.py::make_plugin` -- signature only; lane C fills the
/// body.
pub fn make_plugin(_cfg: &Value) -> Box<dyn ToolPlugin> {
    Box::new(GenericSubprocessTool {
        name: "awk".to_string(),
        binary_candidates: vec!["awk".to_string(), "gawk".to_string(), "mawk".to_string()],
        binary: None,
        env: None,
    })
}
