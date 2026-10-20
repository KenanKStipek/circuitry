//! `plugins/imagemagick.py`, ported -- signature only (M1 gate seam
//! round 2, item S9); lane C fills the body (`make_plugin`'s own
//! binary candidates, `("magick", "convert")`).

use crate::subprocess_tool::GenericSubprocessTool;
use crate::{ToolError, ToolPlugin};
use electricity_value::Value;

/// `plugins/imagemagick.py::make_plugin` -- fallible, like every
/// other `make_plugin`/`_build_*` in this crate (this stub used to
/// be infallible, but
/// Python's own `plugin_env_override` raises on a malformed `runtime.
/// plugins.imagemagick.env`, so a real body here needs `?`, not a
/// function lane C would have to widen the return type of first).
/// Signature only; lane C fills the body.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    Ok(Box::new(GenericSubprocessTool {
        name: "imagemagick".to_string(),
        binary_candidates: vec!["magick".to_string(), "convert".to_string()],
        binary: crate::subprocess_tool::plugin_binary_override(cfg),
        env: crate::subprocess_tool::plugin_env_override(cfg, "imagemagick")?,
    }))
}
