//! Helpers shared by this crate's unit tests.

/// A loopback address nothing answers: port 1, which nothing listens on in
/// CI or on a developer machine, and which is below the ephemeral range,
/// so a parallel test binding `127.0.0.1:0` is never handed it. Binding
/// `127.0.0.1:0` and dropping the listener freed a port that a parallel
/// test's mock server could take and answer instead. (A bound socket that
/// never listens isn't portable: macOS lets a connect to it time out
/// rather than refusing it.)
pub(crate) fn closed_port() -> std::net::SocketAddr {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 1));
    assert!(
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).is_err(),
        "127.0.0.1:1 accepts connections on this machine; these tests need it closed"
    );
    addr
}
