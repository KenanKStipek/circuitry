//! `SystemOnePlugin::execute` -- a proxy value this crate can't use
//! (an unsupported scheme here) is refused outright before any I/O,
//! never silently treated as "no proxy" and sent direct (issue #778,
//! N4; systemone's own twin of `run_curl_bad_proxy.rs`). A separate
//! test binary on purpose: this is the one test in this crate that
//! sets the real `http_proxy` process environment
//! `resolve_proxy_target` reads, and cargo gives each integration-test
//! file its own process, so it can't race any of this crate's own
//! unit tests' real-request calls the way a second test in the same
//! binary could.

use electricity_tools::{ToolPlugin, systemone::make_plugin};
use electricity_value::{CancellationToken, Dict, Value};

fn cfg() -> Value {
    let mut d = Dict::new();
    d.insert(
        Value::Str("base_url".to_string()),
        Value::Str("http://example.invalid".to_string()),
    );
    d.insert(Value::Str("allow_remote".to_string()), Value::Bool(true));
    Value::Dict(d)
}

fn params() -> Value {
    let mut question = Dict::new();
    question.insert(
        Value::Str("type".to_string()),
        Value::Str("noul".to_string()),
    );
    question.insert(
        Value::Str("instructions".to_string()),
        Value::Str("Is it spam?".to_string()),
    );
    let mut questions = Dict::new();
    questions.insert(Value::Str("spam".to_string()), Value::Dict(question));

    let mut state = Dict::new();
    state.insert(
        Value::Str("text".to_string()),
        Value::Str("hello".to_string()),
    );

    let mut d = Dict::new();
    d.insert(
        Value::Str("model".to_string()),
        Value::Str("typesafe-1".to_string()),
    );
    d.insert(Value::Str("state".to_string()), Value::Dict(state));
    d.insert(Value::Str("questions".to_string()), Value::Dict(questions));
    Value::Dict(d)
}

#[tokio::test]
async fn a_bad_proxy_is_refused_before_any_request_reaches_the_server() {
    // SAFETY: the only `#[test]` in this process -- see the module doc.
    unsafe {
        std::env::set_var("http_proxy", "socks5://proxy:1080");
    }
    let plugin = make_plugin(&cfg()).unwrap();
    let token = CancellationToken::new();
    let config = Value::None;
    let outcome = ToolPlugin::execute(
        &*plugin,
        params(),
        &electricity_tools::ToolCall {
            timeout_seconds: 5,
            config: &config,
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
