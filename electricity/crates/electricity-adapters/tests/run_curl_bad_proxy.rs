//! `transport::run_curl` -- the shared request path every curl-
//! replacing adapter in this crate funnels through -- refuses a proxy
//! value it can't use before any I/O (issue #778). A separate test
//! binary on purpose: this is the one test in this crate that sets the
//! real `http_proxy` process environment `resolve_curl_proxy` reads,
//! and cargo gives each integration-test file its own process, so it
//! can't race any other test's own real-request call the way a second
//! test in the same binary could.

use electricity_adapters::transport::{TransportFailure, run_curl};

#[tokio::test]
async fn run_curl_refuses_a_bad_proxy_scheme_before_any_io() {
    // SAFETY: the only `#[test]` in this process -- see the module doc.
    // `no_proxy`/`all_proxy` (either case) are cleared first, so a
    // developer's own shell can't bypass the proxy this test sets or
    // substitute a different one out from under it.
    unsafe {
        std::env::remove_var("no_proxy");
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("all_proxy");
        std::env::remove_var("ALL_PROXY");
        std::env::set_var("http_proxy", "socks5://proxy:1080");
    }
    let result = run_curl("http://example.invalid/x", &[], None, 5).await;
    unsafe {
        std::env::remove_var("http_proxy");
    }

    match result {
        Err(TransportFailure::Validation(message)) => {
            assert!(message.contains("unsupported proxy scheme"), "{message}");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}
