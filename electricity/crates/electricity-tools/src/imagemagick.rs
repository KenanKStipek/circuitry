//! `plugins/imagemagick.py`, ported -- signature only (M1 gate seam
//! round 2, item S9); lane C fills the body (`make_plugin`'s own
//! binary candidates, `("magick", "convert")`).

use crate::ToolPlugin;
use crate::subprocess_tool::GenericSubprocessTool;
use electricity_value::Value;

/// `plugins/imagemagick.py::make_plugin` -- signature only; lane C
/// fills the body.
pub fn make_plugin(_cfg: &Value) -> Box<dyn ToolPlugin> {
    Box::new(GenericSubprocessTool {
        name: "imagemagick".to_string(),
        binary_candidates: vec!["magick".to_string(), "convert".to_string()],
        binary: None,
        env: None,
    })
}
