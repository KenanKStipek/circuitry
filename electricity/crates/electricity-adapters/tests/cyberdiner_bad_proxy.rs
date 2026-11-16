//! `CyberdinerAdapter::generate` -- a proxy value this adapter can't
//! use (an unsupported scheme here) is refused outright before any
//! I/O, never silently treated as "no proxy" and sent direct (issue
//! #778, N4; cyberdiner's own twin of `run_curl_bad_proxy.rs`). A
//! separate test binary on purpose: this is the one test in this crate
//! that sets the real `https_proxy` process environment
//! `resolve_proxy_target` reads, and cargo gives each integration-test
//! file its own process, so it can't race any of this crate's own
//! unit tests' real-request calls the way a second test in the same
//! binary could.

use electricity_adapters::cyberdiner::build_cyberdiner;
use electricity_value::{CancellationToken, Dict, Value};
use electricity_vm::adapter::{CallContext, GenerateOptions};

fn config(expo_url: &str) -> Value {
    let mut d = Dict::new();
    d.insert(
        Value::Str("expo_url".to_string()),
        Value::Str(expo_url.to_string()),
    );
    d.insert(
        Value::Str("token".to_string()),
        Value::Str("ck_test_token".to_string()),
    );
    Value::Dict(d)
}

#[tokio::test]
async fn a_bad_proxy_is_refused_before_any_request_reaches_the_server() {
    // SAFETY: the only `#[test]` in this process -- see the module doc.
    unsafe {
        std::env::set_var("https_proxy", "socks5://proxy:1080");
    }
    let adapter = build_cyberdiner("cyberdiner", &config("https://cyberdiner.invalid")).unwrap();
    let token = CancellationToken::new();
    let path = electricity_bytecode::EffectPath::root();
    let call = CallContext {
        path: &path,
        timeout_seconds: 5,
        token: &token,
        on_progress: None,
    };
    let err = adapter
        .generate("cheap", "hi", &GenerateOptions::default(), &call)
        .await
        .unwrap_err();
    unsafe {
        std::env::remove_var("https_proxy");
    }

    assert!(
        err.message.contains("unsupported proxy scheme"),
        "{}",
        err.message
    );
}
