//! `plugins/awk.py`, ported -- signature only (M1 gate seam round 2,
//! item S9); lane C fills the body (`make_plugin`'s own binary
//! candidates, `("awk", "gawk", "mawk")`).

use crate::subprocess_tool::GenericSubprocessTool;
use crate::{ToolError, ToolPlugin};
use electricity_value::Value;

/// `plugins/awk.py::make_plugin` -- fallible, like every other
/// `make_plugin`/`_build_*` in this crate (review P2-6 on PR #454's
/// second review: this stub used to be infallible, but Python's own
/// `plugin_env_override` raises on a malformed `runtime.plugins.awk.
/// env`, so a real body here needs `?`, not a function lane C would
/// have to widen the return type of first). Signature only; lane C
/// fills the body.
pub fn make_plugin(_cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    Ok(Box::new(GenericSubprocessTool {
        name: "awk".to_string(),
        binary_candidates: vec!["awk".to_string(), "gawk".to_string(), "mawk".to_string()],
        binary: None,
        env: None,
    }))
}
