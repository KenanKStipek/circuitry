//! `HttpPlugin::execute` against a raw loopback server -- the end-to-
//! end shape the conformance harness (Python side) exercises too, but
//! pinned here against hand-written wire responses for cases the
//! mock harness can't express yet (redirects, duplicate headers, a
//! non-UTF-8 body).

use electricity_tools::{CheckResult, ToolCall, ToolPlugin};
use electricity_value::{CancellationToken, Dict, Value};
use std::io::{Read, Write};
use std::net::TcpListener;

fn dict_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_dict()?.get(&Value::Str(key.to_string()))
}

fn params(entries: Vec<(&str, Value)>) -> Value {
    let mut d = Dict::new();
    for (k, v) in entries {
        d.insert(Value::Str(k.to_string()), v);
    }
    Value::Dict(d)
}

static NONE: Value = Value::None;

fn call(token: &CancellationToken) -> ToolCall<'_> {
    ToolCall {
        timeout_seconds: 5,
        config: &NONE,
        token,
        armed: false,
    }
}

fn respond_once(response: &'static [u8]) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(response);
    });
    addr
}

fn record_request(
    response: &'static [u8],
) -> (std::net::SocketAddr, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
        let _ = stream.write_all(response);
    });
    (addr, rx)
}

#[tokio::test]
async fn a_plain_get_returns_the_parsed_json_body() {
    let addr = respond_once(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"ok\": true}\n",
    );
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![("url", Value::Str(format!("http://{addr}/")))]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(result.ok);
    assert_eq!(dict_get(&result.value, "ok"), Some(&Value::Bool(true)));
    let status = dict_get(&result.raw, "status").unwrap();
    assert_eq!(status, &Value::from(200i64));
}

#[tokio::test]
async fn a_404_is_a_returned_failure_with_an_excerpt() {
    let body = b"{\"error\": {\"message\": \"nope\"}}";
    let response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let full: Vec<u8> = [response.as_bytes(), body].concat();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(&full);
    });
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![("url", Value::Str(format!("http://{addr}/")))]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(!result.ok);
    assert_eq!(result.stderr.as_deref(), Some("HTTP 404: Not Found: nope"));
}

#[tokio::test]
async fn a_short_body_on_an_error_response_is_a_returned_failure_not_a_raise() {
    // urllib's own `except HTTPError` handler reads the error body
    // lazily, under `except Exception: text = ""` -- a short/failed
    // read on a 4xx/5xx is a *returned* empty-text result, never a
    // raised `IncompleteRead` (issue #485 review finding 8; contrast
    // `a_short_body_against_content_length_is_incomplete_read` in
    // `http_client_send.rs`, the same short body on a *2xx*, which
    // does still raise).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 10\r\nConnection: close\r\n\r\n12345",
        );
        // Close early, short of the declared Content-Length.
    });
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![("url", Value::Str(format!("http://{addr}/")))]),
        &call(&token),
    )
    .await
    .expect("a short body on an error response is returned, never raised");
    assert!(!result.ok);
    assert_eq!(result.stderr.as_deref(), Some("HTTP 404: Not Found"));
    let body = dict_get(&result.raw, "body").unwrap();
    assert_eq!(body, &Value::Str(String::new()));
}

#[tokio::test]
async fn fail_on_error_false_keeps_ok_true_on_a_500() {
    let addr = respond_once(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/"))),
            ("fail_on_error", Value::Bool(false)),
        ]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(result.ok);
    assert!(result.stderr.is_some());
}

#[tokio::test]
async fn a_post_to_a_303_follows_as_a_get_with_no_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let addr_str = addr.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap();
        tx.send(String::from_utf8_lossy(&buf[..n]).to_string())
            .unwrap();
        let resp = format!(
            "HTTP/1.1 303 See Other\r\nLocation: http://{addr_str}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(resp.as_bytes());

        let (mut stream2, _) = listener.accept().unwrap();
        let n2 = stream2.read(&mut buf).unwrap();
        tx.send(String::from_utf8_lossy(&buf[..n2]).to_string())
            .unwrap();
        let _ = stream2
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    });
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/start"))),
            ("method", Value::Str("POST".to_string())),
            ("body", Value::Str("payload".to_string())),
        ]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(result.ok);
    let first = rx.recv().unwrap();
    assert!(first.starts_with("POST /start"));
    assert!(first.contains("payload"));
    let second = rx.recv().unwrap();
    assert!(second.starts_with("GET /next"));
    assert!(
        !second.contains("content-length") && !second.to_lowercase().contains("content-length")
    );
}

#[tokio::test]
async fn a_post_to_a_307_is_a_success_with_stderr_set() {
    let addr = respond_once(
        b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://example.invalid/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/"))),
            ("method", Value::Str("POST".to_string())),
        ]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(result.ok);
    assert_eq!(
        result.stderr.as_deref(),
        Some("HTTP 307: Temporary Redirect")
    );
}

// Issue #598: `params.redirects`, word for word with `plugins/http.py`.
// Each server below answers one request only, so following a redirect it
// refuses would fail the call outright.

async fn execute_redirect(
    addr: std::net::SocketAddr,
    extra: Vec<(&str, Value)>,
) -> electricity_tools::ToolResult {
    let token = CancellationToken::new();
    let mut entries = vec![("url", Value::Str(format!("http://{addr}/start")))];
    entries.extend(extra);
    ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(entries),
        &call(&token),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn redirects_never_fails_on_a_302_without_following_it() {
    let addr = respond_once(
        b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 5\r\nConnection: close\r\n\r\nmoved",
    );
    let result = execute_redirect(addr, vec![("redirects", Value::Str("never".to_string()))]).await;
    assert!(!result.ok);
    assert_eq!(
        result.stderr.as_deref(),
        Some("HTTP 302: Found: redirect not followed (redirects: never): moved")
    );
}

#[tokio::test]
async fn redirects_never_with_fail_on_error_false_is_a_success() {
    let addr = respond_once(
        b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let result = execute_redirect(
        addr,
        vec![
            ("redirects", Value::Str("never".to_string())),
            ("fail_on_error", Value::Bool(false)),
        ],
    )
    .await;
    assert!(result.ok);
    assert_eq!(
        result.stderr.as_deref(),
        Some("HTTP 302: Found: redirect not followed (redirects: never)")
    );
}

#[tokio::test]
async fn redirects_never_fails_on_a_3xx_that_is_not_a_redirect() {
    let addr = respond_once(
        b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let result = execute_redirect(addr, vec![("redirects", Value::Str("never".to_string()))]).await;
    assert!(!result.ok);
    assert_eq!(result.stderr.as_deref(), Some("HTTP 304: Not Modified"));
}

#[tokio::test]
async fn redirects_strip_auth_never_follows_to_http() {
    let addr = respond_once(
        b"HTTP/1.1 301 Moved Permanently\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let result = execute_redirect(
        addr,
        vec![("redirects", Value::Str("strip_auth".to_string()))],
    )
    .await;
    assert!(!result.ok);
    assert_eq!(
        result.stderr.as_deref(),
        Some(
            "HTTP 301: Moved Permanently: redirect not followed (redirects: strip_auth follows https only)"
        )
    );
}

#[tokio::test]
async fn an_unknown_redirects_value_is_a_value_error() {
    let token = CancellationToken::new();
    let err = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{}/", closed_port()))),
            ("redirects", Value::Str("sometimes".to_string())),
        ]),
        &call(&token),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.message,
        "HttpPlugin: params['redirects'] must be one of 'follow', 'never', 'strip_auth', got 'sometimes'."
    );
}

#[tokio::test]
async fn query_params_with_doseq_append_to_an_existing_query_string() {
    let (addr, rx) =
        record_request(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let token = CancellationToken::new();
    let mut query = Dict::new();
    query.insert(
        Value::Str("tag".to_string()),
        Value::List(vec![
            Value::Str("a".to_string()),
            Value::Str("b".to_string()),
        ]),
    );
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/path?existing=1"))),
            ("params", Value::Dict(query)),
        ]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(result.ok);
    let request_text = rx.recv().unwrap();
    assert!(request_text.starts_with("GET /path?existing=1&tag=a&tag=b"));
}

#[tokio::test]
async fn a_json_body_sets_content_type_and_length() {
    let (addr, rx) =
        record_request(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let token = CancellationToken::new();
    let mut body = Dict::new();
    body.insert(Value::Str("a".to_string()), Value::from(1i64));
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/"))),
            ("method", Value::Str("POST".to_string())),
            ("json", Value::Dict(body)),
        ]),
        &call(&token),
    )
    .await
    .unwrap();
    assert!(result.ok);
    let request_text = rx.recv().unwrap();
    assert!(request_text.contains("Content-Type: application/json"));
    assert!(request_text.contains("{\"a\": 1}"));
}

#[tokio::test]
async fn parse_json_on_a_non_json_body_raises_with_the_decode_text() {
    let addr =
        respond_once(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nnot json");
    let token = CancellationToken::new();
    let err = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/"))),
            ("parse", Value::Str("json".to_string())),
        ]),
        &call(&token),
    )
    .await
    .unwrap_err();
    assert!(
        err.message
            .starts_with("HttpPlugin: parse='json' but response is not JSON:")
    );
}

#[tokio::test]
async fn a_non_utf8_body_decodes_with_replacement_characters() {
    let body: &[u8] = b"\xff\xfe";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let full: Vec<u8> = [response.as_bytes(), body].concat();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(&full);
    });
    let token = CancellationToken::new();
    let result = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![("url", Value::Str(format!("http://{addr}/")))]),
        &call(&token),
    )
    .await
    .unwrap();
    let text = dict_get(&result.raw, "body").unwrap();
    assert_eq!(text, &Value::Str("\u{fffd}\u{fffd}".to_string()));
}

#[tokio::test]
async fn a_connection_refused_raises_the_wrapped_runtime_error() {
    let addr = closed_port();
    let token = CancellationToken::new();
    let url = format!("http://{addr}/");
    let err = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![("url", Value::Str(url.clone()))]),
        &call(&token),
    )
    .await
    .unwrap_err();
    assert!(
        err.message
            .starts_with(&format!("HTTP request to {url} failed:"))
    );
    assert_eq!(err.cause, electricity_tools::ErrorCause::NoReply);
}

#[tokio::test]
async fn save_to_fails_with_the_preview_marker_before_any_request() {
    // Issue #602: `cof run` saves the body to a file; electricity does not.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let token = CancellationToken::new();
    let err = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![
            ("url", Value::Str(format!("http://{addr}/a.png"))),
            ("save_to", Value::Str("a.png".to_string())),
        ]),
        &call(&token),
    )
    .await
    .unwrap_err();
    assert!(err.message.contains(electricity_bytecode::PREVIEW_MARKER));
    assert!(
        err.message
            .contains("an http effect with save_to is not supported")
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn a_blank_url_is_a_value_error() {
    let token = CancellationToken::new();
    let err = ToolPlugin::execute(
        &electricity_tools::http::HttpPlugin,
        params(vec![("url", Value::Str("   ".to_string()))]),
        &call(&token),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.message,
        "HttpPlugin requires params['url'] as a non-empty string."
    );
}

#[test]
fn check_is_trivially_ok() {
    let check: CheckResult = electricity_tools::http::HttpPlugin.check();
    assert!(check.ok);
}

/// A loopback address nothing answers: port 1, which nothing listens on in
/// CI or on a developer machine, and which is below the ephemeral range,
/// so a parallel test binding `127.0.0.1:0` is never handed it. Binding
/// `127.0.0.1:0` and dropping the listener freed a port that a parallel
/// test's mock server could take and answer instead. (A bound socket that
/// never listens isn't portable: macOS lets a connect to it time out
/// rather than refusing it.)
fn closed_port() -> std::net::SocketAddr {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 1));
    assert!(
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).is_err(),
        "127.0.0.1:1 accepts connections on this machine; these tests need it closed"
    );
    addr
}
