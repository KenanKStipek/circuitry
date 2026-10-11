//! `http_client::send` against a raw loopback socket server -- not the
//! mock harness (Python-side, `tests/conformance/mock_http.py`), which
//! only ever runs through `src/http.rs`'s own redirect/header logic;
//! this file pins the client's own wire-level behaviour directly:
//! the server's own (possibly empty) reason phrase, duplicate response
//! headers kept in wire order, and a body read timeout.

use electricity_tools::http_client::{
    self, ConnectTarget, RawRequest, Scheme, SendError, TimeoutModel,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn spawn_raw_server(response: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(response.as_bytes());
    });
    addr
}

fn target(addr: std::net::SocketAddr) -> ConnectTarget {
    ConnectTarget {
        scheme: Scheme::Http,
        host: addr.ip().to_string(),
        port: addr.port(),
        proxy: None,
    }
}

fn get_request() -> RawRequest {
    RawRequest {
        method: "GET".to_string(),
        target: "/".to_string(),
        headers: vec![("Host".to_string(), "test".to_string())],
        body: None,
    }
}

#[tokio::test]
async fn an_empty_reason_phrase_round_trips_verbatim() {
    let addr =
        spawn_raw_server("HTTP/1.1 404 \r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    let client = http_client::build_client().unwrap();
    let response = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 404);
    assert_eq!(response.reason, "");
    assert_eq!(response.body, b"ok");
}

#[tokio::test]
async fn a_non_canonical_reason_phrase_is_kept_verbatim() {
    let addr = spawn_raw_server(
        "HTTP/1.1 200 Everything Is Fine\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let client = http_client::build_client().unwrap();
    let response = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert_eq!(response.reason, "Everything Is Fine");
}

#[tokio::test]
async fn a_canonical_reason_phrase_round_trips_too() {
    let addr =
        spawn_raw_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let client = http_client::build_client().unwrap();
    let response = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert_eq!(response.reason, "OK");
}

#[tokio::test]
async fn duplicate_response_headers_keep_first_position_and_last_value() {
    let addr = spawn_raw_server(
        "HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nX-Other: mid\r\nSet-Cookie: a=2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let client = http_client::build_client().unwrap();
    let response = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let names: Vec<&str> = response.headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec!["set-cookie", "x-other", "content-length", "connection"]
    );
    let set_cookie = response
        .headers
        .iter()
        .find(|(k, _)| k == "set-cookie")
        .map(|(_, v)| v.as_str());
    assert_eq!(set_cookie, Some("a=2"));
}

#[tokio::test]
async fn a_request_is_sent_with_no_default_headers_added() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        let request_text = String::from_utf8_lossy(&buf[..n]).to_string();
        let _ =
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        request_text
    });
    let client = http_client::build_client().unwrap();
    http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let request_text = handle.join().unwrap();
    let header_lines: Vec<&str> = request_text
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    assert_eq!(header_lines, vec!["host: test"]);
}

#[tokio::test]
async fn a_head_response_with_content_length_is_not_an_incomplete_read() {
    // A real regression: a `HEAD` response echoes the `Content-Length`
    // a `GET` to the same path would send, but never actually sends a
    // body on the wire (RFC 9110 §9.3.2) -- `hyper`'s own decoder
    // already knows this and reports a complete, zero-length body;
    // this must not be misread as a short body against that header.
    let addr =
        spawn_raw_server("HTTP/1.1 200 OK\r\nContent-Length: 32\r\nConnection: close\r\n\r\n");
    let client = http_client::build_client().unwrap();
    let request = RawRequest {
        method: "HEAD".to_string(),
        target: "/".to_string(),
        headers: vec![("Host".to_string(), "test".to_string())],
        body: None,
    };
    let response = http_client::send(
        &client,
        &target(addr),
        &request,
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, Vec::<u8>::new());
}

#[tokio::test]
async fn a_short_body_against_content_length_is_incomplete_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n12345");
        // Close early, short of the declared Content-Length.
    });
    let client = http_client::build_client().unwrap();
    let err = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap_err();
    match err {
        SendError::IncompleteRead { read, expected } => {
            assert_eq!(read, 5);
            assert_eq!(expected, 5);
        }
        other => panic!("expected IncompleteRead, got {other:?}"),
    }
}

#[tokio::test]
async fn a_connection_closed_before_any_response_is_remote_disconnected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Drain the client's request first: dropping a socket with
        // unread bytes still sitting in the kernel receive buffer
        // sends an RST, not a graceful FIN -- an inherently racy
        // `ConnectionResetError` on the client side, not the
        // deterministic "closed with nothing sent back at all" this
        // test means to pin. Reading it all away first guarantees a
        // clean close.
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        drop(stream);
    });
    let client = http_client::build_client().unwrap();
    let err = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SendError::RemoteDisconnected));
}

#[tokio::test]
async fn a_slow_body_past_the_deadline_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n12345")
            .unwrap();
        std::thread::sleep(Duration::from_secs(5));
        let _ = stream.write_all(b"67890");
    });
    let client = http_client::build_client().unwrap();
    let err = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_millis(200)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SendError::Timeout));
}

#[tokio::test]
async fn connection_refused_is_a_connect_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let client = http_client::build_client().unwrap();
    let err = http_client::send(
        &client,
        &target(addr),
        &get_request(),
        TimeoutModel::PerOperation(Duration::from_secs(5)),
    )
    .await
    .unwrap_err();
    match err {
        SendError::Connect(text) => assert!(text.contains("Errno")),
        other => panic!("expected Connect, got {other:?}"),
    }
}

#[tokio::test]
async fn dropping_the_send_future_aborts_the_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        // The client writes its request, then this future is dropped
        // before the server ever replies -- a blocking read here
        // returning 0 (EOF) means the client side really did close.
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        let n2 = stream.read(&mut buf).unwrap_or(0);
        let _ = closed_tx.send(n2 == 0);
    });
    let client = http_client::build_client().unwrap();
    {
        let connect_target = target(addr);
        let request = get_request();
        let send_future = http_client::send(
            &client,
            &connect_target,
            &request,
            TimeoutModel::PerOperation(Duration::from_secs(30)),
        );
        // Poll it once so the request is actually written, then drop
        // it without ever awaiting a response.
        let _ = tokio::time::timeout(Duration::from_millis(100), send_future).await;
    }
    // The `#[tokio::test]` runtime is single-threaded: the spawned
    // connection-driver task `AbortOnDrop` just cancelled needs this
    // same runtime to actually poll it once more before the abort
    // takes effect and the socket closes -- `recv_timeout` below is a
    // blocking call, which would otherwise starve that runtime of the
    // chance. Yielding first (still inside the async context) gives it
    // that chance before this test ever blocks the OS thread.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let closed = closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        closed,
        "the server should observe the client closing the connection"
    );
}
