//! `plugins/ffmpeg.py`, ported -- signature only (M1 gate seam round
//! 2, item S9); lane C fills the body. Unlike `awk`/`imagemagick`
//! ([`crate::subprocess_tool::GenericSubprocessTool`]), `ffmpeg` is
//! its own plugin type: Python's own version rejects shell
//! metacharacters in several params fields before dispatch
//! (`_check_safe`/`_check_filter_safe`) and escapes `drawtext` text,
//! neither of which a plain pass-through wrapper can express.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

/// `plugins/ffmpeg.py::FfmpegPlugin` -- signature only; lane C fills
/// the body (the shell-metacharacter rejection, `drawtext` escaping,
/// and the `run_tracked` dispatch itself).
pub struct FfmpegPlugin {
    pub binary: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
}

#[async_trait(?Send)]
impl ToolPlugin for FfmpegPlugin {
    fn name(&self) -> &str {
        "ffmpeg"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "ffmpeg: not implemented yet (lane C)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("ffmpeg: not implemented yet (lane C)".to_string()),
        }
    }
}

/// `plugins/factory.py::_build_ffmpeg` -- fallible, like every other
/// `make_plugin`/`_build_*` in this crate (review P2-6 on PR #454's
/// second review: this stub used to be infallible, but Python's own
/// `plugin_env_override` raises on a malformed `runtime.plugins.
/// ffmpeg.env`, so a real body here needs `?`, not a function lane C
/// would have to widen the return type of first). Signature only;
/// lane C fills the body.
pub fn make_plugin(_cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    Ok(Box::new(FfmpegPlugin {
        binary: None,
        env: None,
    }))
}
