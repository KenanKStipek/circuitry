//! `CyberdinerAdapter::check` through a real plain-`http` proxy (issue
//! #778): the absolute-form request target and `Proxy-Authorization`
//! header a plain (non-tunneled) `http://` proxy connection needs, the
//! same as every other surface this crate and `electricity-tools::http`
//! share the proxy rule with. A separate test binary from `cyberdiner`'s
//! own unit tests on purpose: this is the one test in this crate that
//! sets the real `http_proxy` process environment `resolve_proxy_target`
//! reads, and cargo gives each integration-test file its own process, so
//! it can't race any of those unit tests' own real-request calls the way
//! a second test in the same binary could.

use electricity_adapters::cyberdiner::build_cyberdiner;
use electricity_value::{Dict, Value};
use std::io::{Read, Write};
use std::net::TcpListener;

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
async fn check_over_a_plain_http_proxy_sends_absolute_form_with_proxy_authorization() {
    let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = proxy_listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        tx.send(String::from_utf8_lossy(&buf[..n]).to_string())
            .unwrap();
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    });

    // SAFETY: the only `#[test]` in this process -- see the module doc.
    unsafe {
        std::env::set_var("http_proxy", format!("http://alice:s3cret@{proxy_addr}"));
    }
    let adapter = build_cyberdiner("cyberdiner", &config("http://cyberdiner.invalid")).unwrap();
    let result = adapter.check().await;
    unsafe {
        std::env::remove_var("http_proxy");
    }

    assert!(result.ok, "missing: {:?}", result.missing);
    let request = rx.recv().unwrap();
    let request_line = request.lines().next().unwrap();
    assert!(
        request_line.starts_with("GET http://cyberdiner.invalid"),
        "request line: {request_line:?}"
    );
    assert!(
        request
            .to_lowercase()
            .contains("proxy-authorization: basic")
    );
    // Issue #778: cof's cyberdiner uses urllib, which never adds
    // `Proxy-Connection` on its own -- unlike curl, which this crate's
    // other adapters replay faithfully (`transport::proxy_wire_headers`).
    assert!(!request.to_lowercase().contains("proxy-connection"));
}
