//! The shared HTTP client (#448's decisions: one `reqwest` client,
//! rustls, the system trust store, HTTP/1.1 only, no automatic
//! decompression, no streaming) -- signature only (M1 gate seam round
//! 2, item S9); lane E fills every body. Shared with F2's own
//! adapters, which run raw HTTP to the model providers through the
//! same client rather than curl.

/// Builds the one shared client every HTTP-family tool/adapter
/// dispatches through -- signature only; lane E fills the body.
pub fn build_client() -> Result<(), String> {
    Err("the shared HTTP client is not implemented yet (lane E)".to_string())
}

/// urllib's own redirect rule (the `http` tool's reference; DESIGN §13
/// assumed this everywhere, corrected by #448's decision 1 to curl's
/// own rule for the adapters instead) -- signature only; lane E fills
/// the body.
pub fn redirect_policy() -> Result<(), String> {
    Err("the redirect policy is not implemented yet (lane E)".to_string())
}

/// Proxy resolution (environment `*_PROXY`/`NO_PROXY`, urllib's own
/// precedence) -- signature only; lane E fills the body.
pub fn resolve_proxy(_url: &str) -> Result<Option<String>, String> {
    Err("proxy resolution is not implemented yet (lane E)".to_string())
}
