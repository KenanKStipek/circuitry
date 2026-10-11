//! `plugins/http.py::HttpPlugin`, ported -- signature only (M1 gate
//! seam round 2, item S9); lane E fills every body (the shared
//! `reqwest` client from [`crate::http_client`], urllib's own redirect
//! rule, `execute_tool`'s own HTTP-family status-code/`Retry-After`
//! branch).

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::Value;

pub struct HttpPlugin;

#[async_trait(?Send)]
impl ToolPlugin for HttpPlugin {
    fn name(&self) -> &str {
        "http"
    }

    async fn execute(&self, _params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        Err(ToolError::message(
            "http: not implemented yet (lane E)".to_string(),
        ))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("http: not implemented yet (lane E)".to_string()),
        }
    }
}

/// `plugins/http.py`'s own error-excerpt formatting for a non-2xx
/// response body -- signature only; lane E fills the body.
pub fn http_error_excerpt(_status: u16, _body: &[u8]) -> String {
    "http error excerpt is not implemented yet (lane E)".to_string()
}
