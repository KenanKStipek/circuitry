//! `HttpPlugin::execute` through a real proxy, with a redirect to a host
//! `no_proxy` excludes -- the same scenario `test_http_proxy.py`'s own
//! `test_proxy_credentials_do_not_leak_to_a_redirect_target_that_is_not_proxied`
//! pins on the Python side (issue #778). A separate test binary from
//! `http_plugin.rs` on purpose: this is the one test in this crate that
//! sets the real `http_proxy`/`no_proxy` process environment
//! `resolve_proxy_target` reads, and cargo gives each integration-test
//! file its own process, so it can't race any of that file's many
//! concurrent real-request tests the way a second `#[tokio::test]` in
//! the same binary could.

use electricity_tools::{ToolPlugin, http::HttpPlugin};
use electricity_value::{CancellationToken, Dict, Value};
use std::io::{Read, Write};
use std::net::TcpListener;

fn params(url: &str) -> Value {
    let mut d = Dict::new();
    d.insert(Value::Str("url".to_string()), Value::Str(url.to_string()));
    Value::Dict(d)
}

#[tokio::test]
async fn a_redirect_through_a_proxy_to_a_no_proxy_host_carries_no_proxy_authorization() {
    let origin_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin_addr = origin_listener.local_addr().unwrap();
    let (origin_tx, origin_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = origin_listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        origin_tx
            .send(String::from_utf8_lossy(&buf[..n]).to_string())
            .unwrap();
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    });

    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let (proxy_tx, proxy_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = proxy_listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        proxy_tx
            .send(String::from_utf8_lossy(&buf[..n]).to_string())
            .unwrap();
        let resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{origin_addr}/\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        );
        let _ = stream.write_all(resp.as_bytes());
    });

    // SAFETY: the only `#[test]` in this process -- see the module doc.
    // `NO_PROXY`/`all_proxy`/`ALL_PROXY` are cleared first, so a
    // developer's own shell can't substitute a different proxy decision
    // out from under this test (issue #778).
    unsafe {
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("all_proxy");
        std::env::remove_var("ALL_PROXY");
        std::env::set_var("http_proxy", format!("http://alice:s3cret@{proxy_addr}"));
        std::env::set_var("no_proxy", origin_addr.ip().to_string());
    }
    let token = CancellationToken::new();
    let outcome = ToolPlugin::execute(
        &HttpPlugin,
        params("http://example.invalid/redirect"),
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
        std::env::remove_var("no_proxy");
    }
    let result = outcome.unwrap();

    assert!(result.ok);
    let proxy_request = proxy_rx.recv().unwrap();
    assert!(proxy_request.to_lowercase().contains("proxy-authorization"));
    let origin_request = origin_rx.recv().unwrap();
    assert!(
        !origin_request
            .to_lowercase()
            .contains("proxy-authorization")
    );
}
