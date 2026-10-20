//! The shared HTTP client (#448's decisions: one client stack, rustls
//! with the system trust store, HTTP/1.1 only, no automatic
//! decompression, no streaming) -- lane E (issue #485) fills every
//! body. Shared with lane F2's own model-provider adapters, which run
//! raw HTTP to the providers through the same client rather than curl.
//!
//! Stack decision (issue #485's own "Settled questions"): `hyper` plus
//! `hyper-util` plus `tokio-rustls`, never `reqwest`. Checked
//! empirically against a loopback server before writing anything
//! else, exactly as the issue's own "Settled questions" section asks.
//! `reqwest::Client::builder().build()` inserts an `Accept: */*`
//! default header on every request with no way to suppress it (the
//! mock harness records every header but `User-Agent`, so that one
//! extra header alone would fail every recorded-request comparison
//! against urllib, which never sends one); `reqwest::Response` has no
//! API that exposes a server's own, non-canonical reason phrase at
//! all, only `StatusCode::canonical_reason()`, the RFC text, never
//! what the server actually wrote on the status line.
//!
//! Driving `hyper::client::conn::http1::handshake` directly keeps both
//! working. No default headers are ever added (confirmed against a
//! loopback server: the recorded request carried nothing but `host`).
//! The server's own reason phrase survives as `hyper::ext::ReasonPhrase`
//! on the response's own `Extensions` (also confirmed: an empty reason
//! phrase round-trips as `ReasonPhrase(b"")`, not silently replaced by
//! the canonical text) -- `hyper` only stores that extension when the
//! wire reason differs from the canonical one, so the fallback to
//! `StatusCode::canonical_reason()` below is exact, not approximate,
//! for the common case where the server's own phrase already matches.
//!
//! One connection per request (never pooled: this crate's own model is
//! "one request, maybe a redirect hop, done" -- `hyper-util`'s pooling
//! client (`client-legacy`) solves a problem this lane doesn't have),
//! driven by a task [`send`] spawns and leaves wrapped in an
//! [`AbortOnDrop`] guard for the lifetime of the call: dropping the
//! future `send` returns (`execute_tool`'s own `token.cancelled()`
//! race) drops that guard too, which aborts the still-running
//! connection task and closes the socket -- the cancellation
//! contract the issue's own "Cancellation" bullet asks for, with no
//! `CancellationToken` of this crate's own at all (the caller is the
//! one that races `send`'s future against the token; this module only
//! has to guarantee that losing that race actually tears the
//! connection down, not leak it running in the background).

mod proxy;

pub use proxy::{getproxies_environment, proxy_bypass_environment};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use rustls_pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// `http://` vs `https://` -- whether [`send`] layers TLS on top of the
/// raw TCP (or CONNECT-tunneled) stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

/// Where a request actually connects -- the origin, or a proxy in
/// front of it (urllib's own convention: [`proxy::resolve_proxy`]).
#[derive(Debug, Clone)]
pub struct ConnectTarget {
    pub scheme: Scheme,
    /// The origin's own host -- always the TLS server-name/
    /// certificate-verification target, even through a proxy.
    pub host: String,
    pub port: u16,
    /// `Some` routes the TCP connection through a proxy instead of
    /// straight to *host*/*port*: an `http://` request is forwarded
    /// with an absolute-form request target (the caller's own
    /// [`RawRequest::target`] already has to be the full URL in that
    /// case -- this module never rewrites it); an `https://` request
    /// is tunneled with `CONNECT` first, then TLS layered on top of
    /// the tunnel, to *host*/*port*, exactly as curl/urllib's own
    /// `set_tunnel` both do.
    pub proxy: Option<ProxyTarget>,
}

#[derive(Debug, Clone)]
pub struct ProxyTarget {
    pub host: String,
    pub port: u16,
    /// The already-base64-encoded `user:password` credentials from
    /// the proxy URL's own userinfo (`urllib.request._parse_proxy`),
    /// when present -- sent as `Proxy-Authorization: Basic <this>` on
    /// the `CONNECT` line for a tunneled `https://` request ([`send`]'s
    /// own job: urllib's `do_open` pulls `Proxy-Authorization` out of
    /// the regular header set and *only* ever sends it to the proxy,
    /// never forwarded to the tunneled origin); a plain `http://`
    /// proxied request has no tunnel at all, so [`crate::http`] adds
    /// the same header to the regular request itself instead (that
    /// request *is* the one sent to the proxy).
    pub basic_auth: Option<String>,
}

/// A request already fully built by the caller -- urllib's own
/// `Request` (`src/http.rs`) or, eventually, lane F2's own raw-HTTP
/// adapter call: *target* is exactly the request-target this module
/// puts on the wire (origin-form `/path?query`, or absolute-form when
/// proxied over plain HTTP -- see [`ConnectTarget::proxy`]), *headers*
/// are sent in this exact order and casing, nothing added or
/// reordered.
#[derive(Debug, Clone)]
pub struct RawRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// A response exactly as received -- *reason* is the server's own
/// reason phrase, verbatim, including empty; *headers* keep the wire
/// order of first occurrence and their own casing (lower-cased/
/// deduplicated by the caller, urllib's own rule, not this module's).
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `None` when the body was read in full. `Some` means the body
    /// read failed (a status-line/header-phase failure is still a
    /// plain `Err`, never this): `body` is then always empty
    /// (urllib's own discarded-partial-read, `except Exception: text
    /// = ""`, never the bytes actually read so far) and it is this
    /// response's own caller's job to decide whether that failure
    /// propagates (a 2xx: `with urlopen(...) as resp: resp.read()`
    /// has no handler of its own, so it still raises unwrapped) or is
    /// swallowed into an otherwise-ordinary result (urllib's own
    /// `except HTTPError` handler, reached by a 4xx/5xx or an
    /// unfollowed/disallowed 3xx -- `src/http.rs`'s own job, issue
    /// #485 review finding 8).
    pub body_error: Option<SendError>,
    /// Every body byte actually received before [`RawResponse::body_error`]
    /// (if any) cut the read short -- unlike [`RawResponse::body`], never
    /// cleared. urllib's own discarded-partial-read (the doc comment
    /// above) is specific to the `http` *tool*'s own parity with
    /// `plugins/http.py`; a curl-shelling adapter (`electricity-adapters::
    /// transport::body_error_outcome`) needs the opposite -- real curl
    /// still writes whatever bytes it received to stdout before exiting
    /// nonzero on a dropped connection -- so this field carries that half
    /// forward for that caller alone, and every other caller of
    /// [`send`] that has no use for it just ignores it.
    pub partial_body: Vec<u8>,
}

/// How a timeout applies -- urllib's own per-operation deadline
/// (restarting for connect, then again for every read) versus curl's
/// own `--max-time` (one deadline for the whole call, lane F2's own
/// future caller).
#[derive(Debug, Clone, Copy)]
pub enum TimeoutModel {
    PerOperation(Duration),
    Total(Duration),
}

/// Which phase/cause a [`SendError::Connect`] failure stems from --
/// lane F2's own seam (issue #503's scope table: curl gives DNS
/// failure, connection refused/unreachable, a connect/handshake
/// timeout, a TLS handshake failure and a rejected TLS certificate
/// five different exit codes, so an adapter's own curl-exit mapping
/// needs to tell them apart; the `http` tool never did, since urllib's
/// own `URLError` text is all it ever surfaced). Carried *alongside*
/// [`SendError::Connect`]'s own `Display` text, never derived from it
/// by parsing (issue #503's own ruling: "don't re-derive the kind by
/// parsing error text") -- `Display` and the `http` tool's own output
/// stay byte-for-byte unchanged by this addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectKind {
    /// A DNS lookup for the target host failed.
    Dns,
    /// The connection was refused, or the host/network was
    /// unreachable -- curl's own exit 7 covers both.
    Refused,
    /// The connect phase (TCP connect, TLS handshake, or a proxy
    /// `CONNECT` tunnel) didn't finish before the deadline -- distinct
    /// from [`SendError::Timeout`], which is the *unwrapped* shape a
    /// status-line/body-read timeout raises instead.
    Timeout,
    /// A TLS handshake failed for a reason other than the peer's
    /// certificate (a protocol/version mismatch, ...).
    TlsHandshake,
    /// The peer's TLS certificate was rejected (unknown issuer,
    /// expired, hostname mismatch, ...) -- curl's own exit 60, the one
    /// connect-phase failure that is *not* retried.
    TlsCertificate,
    /// A failed proxy `CONNECT` tunnel, or any other connect-phase
    /// failure this crate doesn't classify more precisely -- curl's
    /// own generic "couldn't connect" exit 7.
    Other,
}

/// Everything [`send`] can fail with, already shaped for
/// [`crate::http::HttpPlugin`]'s own raise-vs-return split (issue
/// #485's own "Raised failures" section): *Connect* is urllib's own
/// `URLError`-wrapped text (DNS, refused, a connect-phase timeout, a
/// TLS failure); *Timeout*/*RemoteDisconnected*/*IncompleteRead* are
/// the three raised-but-*unwrapped* shapes, surfacing while waiting
/// for (or reading) the response rather than while connecting/sending.
#[derive(Debug, Clone)]
pub enum SendError {
    /// Formatted as CPython's own `OSError`/`ssl` text would be, by
    /// *location* (ruling 5): `[Errno N] <strerror>` for an OS error,
    /// the raw TLS error text otherwise. *kind* (issue #503's own
    /// seam) lets a caller map this to curl's own differently-coded
    /// exit status without re-parsing `text`.
    Connect { kind: ConnectKind, text: String },
    /// A timeout waiting for the status line, or while reading the
    /// body -- always the literal text `"timed out"` (`socket.timeout`/
    /// `TimeoutError`'s own `str()`), unwrapped.
    Timeout,
    /// The connection closed before a status line ever arrived.
    RemoteDisconnected,
    /// The connection closed after a `Content-Length` response began,
    /// short of the declared length.
    IncompleteRead { read: usize, expected: usize },
    /// Any other failure (a malformed response, an invalid request
    /// target too late to catch at parse time, ...) -- `Display`'s own
    /// text is compared by location, never exactly.
    Other(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Connect { text, .. } => write!(f, "{text}"),
            SendError::Timeout => write!(f, "timed out"),
            SendError::RemoteDisconnected => {
                write!(f, "Remote end closed connection without response")
            }
            SendError::IncompleteRead { read, expected } => {
                write!(
                    f,
                    "IncompleteRead({read} bytes read, {expected} more expected)"
                )
            }
            SendError::Other(text) => write!(f, "{text}"),
        }
    }
}

/// `os.strerror(code)` -- the same C `strerror` urllib's own `OSError`
/// text is built from, via the `libc` crate already in this
/// workspace's dependency tree (shared with `fs.rs`'s own OS-error
/// formatter, which this function deliberately mirrors rather than
/// importing across a lane boundary: `fs.rs` is D1's own file).
fn os_strerror(code: i32) -> String {
    // Safety: `strerror` always returns a valid, null-terminated
    // pointer into a statically-owned (or thread-local, platform-
    // dependent) buffer for any `code` -- never null, per POSIX.
    unsafe {
        let ptr = libc::strerror(code);
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

/// `[Errno N] <strerror>` -- CPython's own `OSError.__str__` for an
/// error that carries an `errno` (`io::Error::raw_os_error`); anything
/// else (no OS errno at all -- a `rustls`/protocol-level failure) just
/// keeps Rust's own `Display` text, compared by location (ruling 5),
/// never exactly, same as an OS error's own `strerror` text already is.
fn format_connect_error(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => format!("[Errno {code}] {}", os_strerror(code)),
        None => err.to_string(),
    }
}

/// Builds the shared `rustls` client config every HTTP-family tool/
/// adapter dispatches through -- the system trust store
/// (`rustls-native-certs`, #448 decision 1: never a bundled CA list, so
/// a locally-installed corporate root works here exactly as it does
/// for `cof run`'s own TLS stack), HTTP/1.1 only (an explicit ALPN
/// list of just `http/1.1`, so a server that would otherwise speak h2
/// over TLS still negotiates down -- #448 decision 1 forbids HTTP/2
/// outright), no client certificate.
///
/// Built once per process and cached in [`CACHED_CLIENT`]: every
/// caller (one per tool/adapter attempt, even for a plain `http://`
/// request that never touches TLS at all) shares the one trust-store
/// read, matching [`HttpClient`]'s own doc comment below.
pub fn build_client() -> Result<HttpClient, String> {
    CACHED_CLIENT.get_or_init(build_client_uncached).clone()
}

static CACHED_CLIENT: std::sync::OnceLock<Result<HttpClient, String>> = std::sync::OnceLock::new();

fn build_client_uncached() -> Result<HttpClient, String> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for err in &native.errors {
        log::warn!("http_client: skipping an unreadable native certificate: {err}");
    }
    for cert in native.certs {
        // A malformed native cert is skipped, not fatal -- the same
        // tolerance `rustls-native-certs`'s own `load_native_certs`
        // already documents for an unreadable *source* (above); an
        // individual cert that merely fails to parse is rarer still
        // and no more fatal to the overall trust store.
        let _ = roots.add(cert);
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(HttpClient {
        tls_config: Arc::new(config),
    })
}

/// The one shared client handle -- cheap to clone (an `Arc` around the
/// TLS config), so a per-attempt tool/adapter call can hold its own
/// copy without re-reading the system trust store every time.
#[derive(Clone)]
pub struct HttpClient {
    tls_config: Arc<rustls::ClientConfig>,
}

/// Aborts the wrapped connection-driver task on drop -- see this
/// module's own crate doc comment above for why that's the whole of
/// this module's cancellation contract.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum AnyStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for AnyStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            AnyStream::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            AnyStream::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for AnyStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            AnyStream::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            AnyStream::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            AnyStream::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            AnyStream::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            AnyStream::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            AnyStream::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Connects to *target*'s own TCP endpoint (the origin, or a proxy),
/// tunnels and/or layers TLS as *target* asks for, and returns the one
/// stream [`send`] speaks HTTP/1.1 over. *connect_deadline* is only
/// ever a single, one-shot deadline -- urllib's own per-operation
/// timeout model restarts for each read afterward, which is [`send`]'s
/// own job, not this function's.
async fn connect(
    client: &HttpClient,
    target: &ConnectTarget,
    connect_deadline: Duration,
) -> Result<AnyStream, SendError> {
    let (tcp_host, tcp_port) = match &target.proxy {
        Some(proxy) => (proxy.host.as_str(), proxy.port),
        None => (target.host.as_str(), target.port),
    };
    // A connect-phase timeout is urllib's own `OSError`-wrapped shape
    // too (`h.request()`'s own `except OSError as err: raise
    // URLError(err)`, `socket.timeout`'s own `str()` being just `timed
    // out`), never the unwrapped `TimeoutError` [`send`]'s own
    // status-line/body-read timeouts raise -- [`SendError::Connect`],
    // not [`SendError::Timeout`] (issue #485 review finding 2).
    let tcp = tokio::time::timeout(connect_deadline, TcpStream::connect((tcp_host, tcp_port)))
        .await
        .map_err(|_| SendError::Connect {
            kind: ConnectKind::Timeout,
            text: "timed out".to_string(),
        })
        .and_then(|r| {
            r.map_err(|e| SendError::Connect {
                kind: classify_connect_io_error(&e),
                text: format_connect_error(&e),
            })
        })?;
    tcp.set_nodelay(true).ok();

    let tcp = if let Some(proxy) = target
        .proxy
        .as_ref()
        .filter(|_| target.scheme == Scheme::Https)
    {
        connect_tunnel(
            tcp,
            &target.host,
            target.port,
            proxy.basic_auth.as_deref(),
            connect_deadline,
        )
        .await?
    } else {
        tcp
    };

    match target.scheme {
        Scheme::Http => Ok(AnyStream::Plain(tcp)),
        Scheme::Https => {
            let connector = TlsConnector::from(client.tls_config.clone());
            let server_name =
                ServerName::try_from(target.host.clone()).map_err(|_| SendError::Connect {
                    kind: ConnectKind::Other,
                    text: format!("invalid TLS server name '{}'", target.host),
                })?;
            let tls = tokio::time::timeout(connect_deadline, connector.connect(server_name, tcp))
                .await
                .map_err(|_| SendError::Connect {
                    kind: ConnectKind::Timeout,
                    text: "timed out".to_string(),
                })
                .and_then(|r| {
                    r.map_err(|e| SendError::Connect {
                        kind: classify_tls_io_error(&e),
                        text: e.to_string(),
                    })
                })?;
            Ok(AnyStream::Tls(Box::new(tls)))
        }
    }
}

/// Tells a DNS lookup failure apart from "connection refused"/"host
/// unreachable" in a [`TcpStream::connect`] failure -- curl's own exit
/// 6 versus 7. Rust's std has no dedicated `io::ErrorKind` for a DNS
/// failure (unlike `ConnectionRefused`): a lookup failure surfaces
/// with no OS errno attached at all (`std::net::ToSocketAddrs`'s own
/// resolver wraps `getaddrinfo`'s own non-success code as a plain
/// `io::Error::new(ErrorKind::Uncategorized/Other, ...)`, never a
/// `raw_os_error`), which every real connect-phase OS error (refused,
/// unreachable, a routing failure) always carries -- that absence is
/// the signal, not a text match on the message.
fn classify_connect_io_error(err: &std::io::Error) -> ConnectKind {
    if err.kind() == std::io::ErrorKind::ConnectionRefused {
        return ConnectKind::Refused;
    }
    if err.raw_os_error().is_none() {
        return ConnectKind::Dns;
    }
    ConnectKind::Refused
}

/// Tells a rejected TLS certificate apart from every other TLS
/// handshake failure -- curl's own exit 60 (not retried) versus 35
/// (retried). `tokio_rustls::TlsConnector::connect`'s own failure is
/// always an `io::Error` wrapping the underlying [`rustls::Error`] as
/// its inner source (`rustls::Error` implements `std::error::Error`,
/// so this is a plain downcast, never a text match on `Display`).
fn classify_tls_io_error(err: &std::io::Error) -> ConnectKind {
    let is_certificate_error = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|tls_err| matches!(tls_err, rustls::Error::InvalidCertificate(_)));
    if is_certificate_error {
        ConnectKind::TlsCertificate
    } else {
        ConnectKind::TlsHandshake
    }
}

/// `CONNECT host:port HTTP/1.1` against an already-connected proxy
/// stream, urllib/curl's own shared tunneling handshake -- raw text,
/// not a `hyper` request, since a `CONNECT` reply is never a normal
/// HTTP response body the rest of this module's own parsing expects
/// (the tunnel's *first* real response, after TLS, is).
async fn connect_tunnel(
    mut tcp: TcpStream,
    host: &str,
    port: u16,
    basic_auth: Option<&str>,
    deadline: Duration,
) -> Result<TcpStream, SendError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let auth_line = basic_auth
        .map(|creds| format!("Proxy-Authorization: Basic {creds}\r\n"))
        .unwrap_or_default();
    let request =
        format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n{auth_line}\r\n");
    tokio::time::timeout(deadline, tcp.write_all(request.as_bytes()))
        .await
        .map_err(|_| SendError::Connect {
            kind: ConnectKind::Timeout,
            text: "timed out".to_string(),
        })
        .and_then(|r| {
            r.map_err(|e| SendError::Connect {
                kind: classify_connect_io_error(&e),
                text: format_connect_error(&e),
            })
        })?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let n = tokio::time::timeout(deadline, tcp.read(&mut chunk))
            .await
            .map_err(|_| SendError::Connect {
                kind: ConnectKind::Timeout,
                text: "timed out".to_string(),
            })
            .and_then(|r| {
                r.map_err(|e| SendError::Connect {
                    kind: classify_connect_io_error(&e),
                    text: format_connect_error(&e),
                })
            })?;
        if n == 0 {
            return Err(SendError::RemoteDisconnected);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let status_line = buf
        .split(|&b| b == b'\n')
        .next()
        .map(|l| String::from_utf8_lossy(l).trim().to_string())
        .unwrap_or_default();
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .map(|code| (200..300).contains(&code))
        .unwrap_or(false);
    if !ok {
        // A failed tunnel is urllib's own `OSError`-wrapped shape too
        // (`http.client.HTTPConnection._tunnel`'s own `OSError` on a
        // non-2xx `CONNECT` reply, caught and re-raised as `URLError`
        // by the same `except OSError` in `h.request()` [`connect`]'s
        // own doc comment above already describes) -- retryable, not
        // [`SendError::Other`] (issue #485 review finding 7).
        return Err(SendError::Connect {
            kind: ConnectKind::Other,
            text: format!("proxy CONNECT to {host}:{port} failed: {status_line}"),
        });
    }
    Ok(tcp)
}

/// Sends *request* over a fresh connection to *target*, applying
/// *timeout* (urllib's own per-operation model, or curl's own total
/// one), and returns the full response (status, reason, headers,
/// body) -- never streamed, never decompressed (#448 decision 1).
/// `Total` wraps the *entire* call (connect through the last body
/// byte) in one `tokio::time::timeout`, on top of [`send_inner`]'s
/// own per-phase deadlines of the same duration -- each phase alone
/// can never exceed the total, and this outer wrapper is what catches
/// several phases each individually under the limit but summing past
/// it (issue #485; this lane's own caller,
/// `src/http.rs`, never builds `Total`, only lane F2's adapters do).
pub async fn send(
    client: &HttpClient,
    target: &ConnectTarget,
    request: &RawRequest,
    timeout: TimeoutModel,
) -> Result<RawResponse, SendError> {
    match timeout {
        TimeoutModel::Total(d) => {
            tokio::time::timeout(d, send_inner(client, target, request, timeout))
                .await
                .unwrap_or(Err(SendError::Timeout))
        }
        TimeoutModel::PerOperation(_) => send_inner(client, target, request, timeout).await,
    }
}

async fn send_inner(
    client: &HttpClient,
    target: &ConnectTarget,
    request: &RawRequest,
    timeout: TimeoutModel,
) -> Result<RawResponse, SendError> {
    let (connect_deadline, response_deadline, body_deadline) = match timeout {
        TimeoutModel::PerOperation(d) => (d, d, d),
        TimeoutModel::Total(d) => (d, d, d),
    };

    let stream = connect(client, target, connect_deadline).await?;
    let io = TokioIo::new(stream);
    // urllib title-cases every header name on the wire
    // (`http.client.HTTPConnection.putheader`'s own
    // `header.title()`-equivalent, `Accept-Encoding`/`Content-Type`/
    // an arbitrary `X-Custom-Header` alike); `hyper`'s own default is
    // lower-case. Header *order* on the wire is not required to
    // match (the mock harness compares by name, case-insensitively).
    let handshake = http1::Builder::new().title_case_headers(true).handshake(io);
    let (mut sender, conn) = tokio::time::timeout(connect_deadline, handshake)
        .await
        .map_err(|_| SendError::Connect {
            kind: ConnectKind::Timeout,
            text: "timed out".to_string(),
        })?
        .map_err(|e| SendError::Connect {
            kind: ConnectKind::Other,
            text: e.to_string(),
        })?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    let _guard = AbortOnDrop(handle);

    let mut builder = http::Request::builder()
        .method(request.method.as_str())
        .uri(request.target.as_str());
    for (name, value) in &request.headers {
        // `http.client`'s own `putheader` encode step is latin-1, not
        // UTF-8: every header value this crate ever builds has
        // already been checked latin-1-encodable at the caller
        // (`http.rs`'s own `first_non_latin1_char`), so each `char`'s
        // own codepoint (<= 0xFF) *is* its one-byte latin-1 encoding
        // -- `HeaderValue::from_str` would instead UTF-8-encode a
        // non-ASCII codepoint into two or more bytes, putting the
        // wrong bytes on the wire (issue #485 review finding 3).
        let latin1_bytes: Vec<u8> = value.chars().map(|c| c as u8).collect();
        let header_value = http::HeaderValue::from_bytes(&latin1_bytes)
            .map_err(|e| SendError::Other(e.to_string()))?;
        builder = builder.header(name.as_str(), header_value);
    }
    let body = match &request.body {
        Some(bytes) => Full::new(Bytes::from(bytes.clone())).boxed(),
        None => Empty::<Bytes>::new().boxed(),
    };
    let http_request = builder
        .body(body)
        .map_err(|e| SendError::Other(e.to_string()))?;

    let response = tokio::time::timeout(response_deadline, sender.send_request(http_request))
        .await
        .map_err(|_| SendError::Timeout)?
        .map_err(classify_response_phase_error)?;

    let status = response.status().as_u16();
    let reason = response
        .extensions()
        .get::<hyper::ext::ReasonPhrase>()
        .and_then(|r| std::str::from_utf8(r.as_bytes()).ok())
        .map(str::to_string)
        .unwrap_or_else(|| {
            response
                .status()
                .canonical_reason()
                .unwrap_or("")
                .to_string()
        });
    // `{k.lower(): v for k, v in resp.headers.items()}` (`plugins/
    // http.py`): a duplicated name keeps its *first* position but its
    // *last* value -- built here, not left to the caller, since `http`
    // crate's own `HeaderName` already always lower-cases on storage
    // (so there is no original casing left to preserve anyway) and its
    // `HeaderMap::iter()` groups a repeated name's values together at
    // that name's first slot rather than their true interleaved wire
    // position (confirmed directly; this lane's own notes warn not to
    // rely on it) -- harmless for *this* dedup, which only ever needs
    // "first position, last value", never the interleaving itself.
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in response.headers() {
        let name = name.as_str().to_string();
        // `http.client`'s own header decode is latin-1 (`HTTPMessage`
        // is parsed with `email.parser`'s own `BytesHeaderParser`,
        // itself latin-1), not UTF-8 -- each byte becomes the
        // codepoint of that same ordinal, never
        // `String::from_utf8_lossy`'s multi-byte decode/replacement
        // (issue #485 review finding 3; this also makes a later
        // redirect's own `Location` re-quoting agree with urllib's,
        // since [`http_url::quote`] already treats a codepoint <=
        // 0xFF as the one byte to percent-encode).
        let value: String = value.as_bytes().iter().map(|&b| b as char).collect();
        match headers.iter_mut().find(|(k, _)| *k == name) {
            Some(entry) => entry.1 = value,
            None => headers.push((name, value)),
        }
    }
    let content_length: Option<usize> = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    let mut incoming: Incoming = response.into_body();
    let mut body_bytes: Vec<u8> = Vec::new();
    // A failure reading the body -- urllib's own `exc.read()`, called
    // *lazily* by the plugin's own `except HTTPError` handler only
    // after the status/headers (and the redirect-follow decision, for
    // a 3xx) are already known, under `except Exception: text = ""`.
    // `http.client`'s own `with urlopen(...) as resp: ... resp.read()`
    // has no such handler for a *2xx* response, so the same failure
    // there still propagates unwrapped. This function can't tell
    // which case it is (that's `src/http.rs`'s own job, once it knows
    // the status and the redirect decision) -- so it never raises on
    // a body-phase failure once the status line and headers already
    // arrived; it reports the failure in [`RawResponse::body_error`]
    // instead and lets the body stand as empty (matching Python's own
    // discarded-partial-read exactly, never the bytes read so far).
    let mut body_error: Option<SendError> = None;
    loop {
        let frame = match tokio::time::timeout(body_deadline, incoming.frame()).await {
            Ok(frame) => frame,
            Err(_) => {
                body_error = Some(SendError::Timeout);
                break;
            }
        };
        match frame {
            None => break,
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    body_bytes.extend_from_slice(chunk);
                }
            }
            Some(Err(err)) => {
                body_error = Some(classify_body_phase_error(
                    err,
                    body_bytes.len(),
                    content_length,
                ));
                break;
            }
        }
    }
    // A response to `HEAD` never carries a body on the wire, even when
    // its own `Content-Length` echoes what a `GET` to the same path
    // would have sent (RFC 9110 §9.3.2) -- `hyper`'s own decoder
    // already knows this (it tracks the request method internally and
    // treats the body as already complete, zero-length, regardless of
    // the header), so the frame loop above correctly read nothing; this
    // check would otherwise misreport that as a short body. `http.
    // client` makes the same exception for 1xx/204/304 (RFC 9110
    // §6.4.1/§8.6: a response in any of these classes never has a
    // body even when `Content-Length` is still present).
    let never_has_a_body = request.method == "HEAD" || matches!(status, 100..=199 | 204 | 304);
    if body_error.is_none() {
        if let Some(expected) = content_length {
            if !never_has_a_body && body_bytes.len() < expected {
                body_error = Some(SendError::IncompleteRead {
                    read: body_bytes.len(),
                    expected: expected - body_bytes.len(),
                });
            }
        }
    }
    // Only a body-phase failure's own caller (`electricity-adapters::
    // transport::body_error_outcome`) ever reads `partial_body` at all
    // (review finding F11) -- the success path leaves it empty rather
    // than cloning the whole body for nothing.
    let partial_body = if body_error.is_some() {
        std::mem::take(&mut body_bytes)
    } else {
        Vec::new()
    };

    Ok(RawResponse {
        status,
        reason,
        headers,
        body: body_bytes,
        body_error,
        partial_body,
    })
}

/// A `send_request` failure, after a successful connect -- urllib's
/// own unwrapped shapes (waiting for the status line): a connection
/// closed before any response at all is [`SendError::RemoteDisconnected`];
/// anything else keeps `hyper`'s own text, compared by location.
fn classify_response_phase_error(err: hyper::Error) -> SendError {
    // A peer that closes with no unread bytes left in its own receive
    // buffer sends a graceful FIN (`is_incomplete_message`/`is_closed`
    // below); one that still has unread bytes sitting there sends an
    // RST instead, which surfaces as a plain `io::ErrorKind::
    // ConnectionReset` -- a real race any TCP peer can hit depending
    // on kernel buffering/scheduling, not a distinction `plugins/
    // http.py`'s own urllib-based reference actually makes a retry
    // decision on (`adapters/_retry.py::_classify_cause_chain`'s own
    // `ConnectionError` arm already treats *either* text as
    // `ErrorCause::NoReply`, i.e. retryable) -- so this crate folds
    // both into the one [`SendError::RemoteDisconnected`] shape rather
    // than exposing the raw OS text for a condition whose wording a
    // real server's own timing, not this client, decides.
    if err.is_incomplete_message() || err.is_closed() || is_connection_reset(&err) {
        SendError::RemoteDisconnected
    } else {
        SendError::Other(err.to_string())
    }
}

fn is_connection_reset(err: &dyn std::error::Error) -> bool {
    let mut cause = err.source();
    while let Some(err) = cause {
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) {
                return true;
            }
        }
        cause = err.source();
    }
    false
}

/// A body-read failure -- a short body against a known
/// `Content-Length` is [`SendError::IncompleteRead`] (not retried, per
/// issue #485); anything else keeps `hyper`'s own text.
fn classify_body_phase_error(
    err: hyper::Error,
    read_so_far: usize,
    content_length: Option<usize>,
) -> SendError {
    // `IncompleteMessage` (`Error::is_incomplete_message`) only ever
    // covers the message *head* (status line/headers) going short --
    // a `Content-Length`-framed *body* ending early instead surfaces
    // as an `io::ErrorKind::UnexpectedEof` wrapped as `Kind::Body`
    // (confirmed directly against `hyper`'s own `decode.rs`), with no
    // public `is_*` accessor of its own, only reachable through the
    // `source()` chain `std::error::Error` already guarantees.
    let unexpected_eof = is_unexpected_eof(&err) || err.is_incomplete_message();
    if unexpected_eof {
        match content_length {
            Some(expected) if expected > read_so_far => SendError::IncompleteRead {
                read: read_so_far,
                expected: expected - read_so_far,
            },
            _ => SendError::RemoteDisconnected,
        }
    } else {
        SendError::Other(err.to_string())
    }
}

fn is_unexpected_eof(err: &dyn std::error::Error) -> bool {
    let mut cause = err.source();
    while let Some(err) = cause {
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                return true;
            }
        }
        cause = err.source();
    }
    false
}

/// What a 3xx response does to the next request, urllib's own
/// `HTTPRedirectHandler.redirect_request` method/status matrix
/// (`src/http.rs`'s own caller applies this once per hop, resolving
/// the new URL and dropping/keeping headers itself -- this function is
/// only the method-transition decision, the one part of the rule that
/// doesn't need the URL or the header set at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectRequest {
    pub method: String,
    /// The redirected request has no body, and drops
    /// `Content-Length`/`Content-Type` -- always `true` when
    /// [`redirect_policy`] returns `Some` at all.
    pub drop_body: bool,
}

/// `None` when *method*/*status* isn't followed at all (surfaces as a
/// returned result with that status instead, `src/http.rs`'s own
/// job) -- 301/302/303/307/308 for `GET`/`HEAD`, 301/302/303 for
/// `POST`; any other method/status pair is never followed.
///
/// The followed request is always `GET`, including for a `HEAD`
/// (issue #485's own open question, pinned by
/// `generate_http_corpus.py`'s `redirect_decision_cases`, checked
/// directly against CPython: a real loopback HEAD-then-307 round
/// trip sends GET on the second request, confirmed empirically, not
/// assumed). `HTTPRedirectHandler.redirect_request` builds the
/// follow-up `Request` with no `method=`/`data=` of its own, and
/// `Request.get_method()`'s own fallback for that shape is
/// unconditionally `"GET"` -- the original method only ever gated
/// *whether* a redirect is followed at all, never *what* the
/// follow-up request's own method is.
pub fn redirect_policy(method: &str, status: u16) -> Option<RedirectRequest> {
    let method = method.to_ascii_uppercase();
    let get_or_head_redirect = matches!(status, 301..=303 | 307 | 308);
    let post_redirect = matches!(status, 301..=303);
    let followed = match method.as_str() {
        "GET" | "HEAD" => get_or_head_redirect,
        "POST" => post_redirect,
        _ => false,
    };
    if !followed {
        return None;
    }
    Some(RedirectRequest {
        method: "GET".to_string(),
        drop_body: true,
    })
}

/// `urllib.request.getproxies_environment`/`proxy_bypass_environment`'s
/// own composition (#448 decision 1) -- `None` when there's no proxy
/// configured for *scheme* or `no_proxy` bypasses *host*. Reads the
/// real process environment (`std::env::vars()`); every test that
/// calls this (directly, or through [`crate::http::HttpPlugin`]) sets
/// or clears every `*_proxy`/`*_PROXY`/`REQUEST_METHOD` variable
/// explicitly first, per this lane's own environment-isolation rule.
pub fn resolve_proxy(scheme: &str, host: &str) -> Option<String> {
    let env: Vec<(String, String)> = std::env::vars().collect();
    proxy::resolve_proxy(scheme, host, &env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_policy_follows_get_and_head_for_every_redirect_status() {
        for status in [301, 302, 303, 307, 308] {
            assert!(redirect_policy("GET", status).is_some());
            assert!(redirect_policy("HEAD", status).is_some());
        }
    }

    #[test]
    fn redirect_policy_always_follows_with_get_even_for_a_head_request() {
        // Confirmed against real CPython (`generate_http_corpus.py`'s
        // own `redirect_decision_cases`, and a live loopback round
        // trip): `HTTPRedirectHandler.redirect_request` never carries
        // the original method forward, for any status, including 307/
        // 308 where common intuition expects a `HEAD` to stay a `HEAD`.
        assert_eq!(redirect_policy("HEAD", 307).unwrap().method, "GET");
        assert_eq!(redirect_policy("HEAD", 308).unwrap().method, "GET");
        assert_eq!(redirect_policy("GET", 308).unwrap().method, "GET");
    }

    #[test]
    fn redirect_policy_narrows_301_302_303_to_get_even_for_head() {
        assert_eq!(redirect_policy("HEAD", 301).unwrap().method, "GET");
    }

    #[test]
    fn redirect_policy_follows_post_only_for_301_302_303() {
        for status in [301, 302, 303] {
            assert_eq!(redirect_policy("POST", status).unwrap().method, "GET");
        }
        assert!(redirect_policy("POST", 307).is_none());
        assert!(redirect_policy("POST", 308).is_none());
    }

    #[test]
    fn redirect_policy_never_follows_an_unlisted_status() {
        assert!(redirect_policy("GET", 200).is_none());
        assert!(redirect_policy("GET", 304).is_none());
    }

    #[test]
    fn build_client_succeeds_on_a_normal_host() {
        assert!(build_client().is_ok());
    }
}
