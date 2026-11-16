//! `HttpPlugin::execute` -- a proxy value it can't use (an unsupported
//! scheme here) is refused outright before any I/O, never silently
//! treated as "no proxy" and sent direct (issue #778; the `http`
//! tool's own twin of `run_curl_bad_proxy.rs`). A separate test binary
//! on purpose: this is the one test in this crate that sets the real
//! `http_proxy` process environment `resolve_proxy_target` reads, and
//! cargo gives each integration-test file its own process, so it can't
//! race any other test's own real-request call the way a second test
//! in the same binary could.

use electricity_tools::{ToolPlugin, http::HttpPlugin};
use electricity_value::{CancellationToken, Dict, Value};

fn params(url: &str) -> Value {
    let mut d = Dict::new();
    d.insert(Value::Str("url".to_string()), Value::Str(url.to_string()));
    Value::Dict(d)
}

#[tokio::test]
async fn a_bad_proxy_is_refused_before_any_request_reaches_the_server() {
    // SAFETY: the only `#[test]` in this process -- see the module doc.
    unsafe {
        std::env::set_var("http_proxy", "socks5://proxy:1080");
    }
    let token = CancellationToken::new();
    let outcome = ToolPlugin::execute(
        &HttpPlugin,
        params("http://example.invalid/x"),
        &electricity_tools::ToolCall {
            timeout_seconds: 5,
            config: &Value::None,
            token: &token,
            armed: false,
            secret_headers: &[],
            scrub: None,
            on_live: None,
            on_agent_live: None,
            agent_attempt: 1,
            agent_earlier_attempts: None,
            limited: false,
            confine_to: None,
        },
    )
    .await;
    unsafe {
        std::env::remove_var("http_proxy");
    }

    let err = outcome.unwrap_err();
    assert!(
        err.message.contains("unsupported proxy scheme"),
        "{}",
        err.message
    );
}
