//! Socket servers for the line framing: newline-delimited JSON-RPC over TCP
//! and Unix domain sockets.
//!
//! These reuse [`LineTransport`] — the same framing stdio speaks — so any MCP
//! client that can open a socket and write line-delimited JSON works
//! unchanged. Each accepted connection gets its **own service** from the
//! `make_service` factory: the legacy (`2025-11-25`) path binds a session per
//! connection (`LegacySessionAdapter`), so sharing one service value across
//! connections would fuse their sessions.
//!
//! The accept loops stop when the [`ServeConfig::shutdown`] token fires; each
//! live connection then drains through the serve driver's two-phase drain.
//!
//! TLS: terminate at your ingress/proxy, or drive
//! [`LineTransport`] over a TLS stream yourself with
//! [`turbomcp_service::serve_with`] — these helpers serve plaintext sockets
//! (the common case: localhost, private networks, and proxied deployments).

use tokio::io::BufReader;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use turbomcp_codec::DefaultCodec;
use turbomcp_service::{McpService, ServeConfig};

use crate::{LineTransport, StdioError};

/// Serve line-framed JSON-RPC on a TCP listener with the default
/// [`ServeConfig`], building one service per connection from `make_service`
/// (e.g. `move || LegacySessionAdapter::new(dispatcher.clone())`).
///
/// # Errors
/// Returns only if accepting a connection fails; per-connection errors are
/// logged and do not stop the accept loop.
pub async fn serve_tcp<S, F>(listener: TcpListener, make_service: F) -> Result<(), StdioError>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: McpService + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    serve_tcp_with(listener, make_service, ServeConfig::default()).await
}

/// [`serve_tcp`] with an explicit [`ServeConfig`] applied to each connection;
/// its shutdown token also stops the accept loop.
///
/// # Errors
/// Returns only if accepting a connection fails.
pub async fn serve_tcp_with<S, F>(
    listener: TcpListener,
    make_service: F,
    config: ServeConfig,
) -> Result<(), StdioError>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: McpService + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let shutdown = config.shutdown.clone();
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted,
        };
        let (socket, peer) = accepted?;
        let (rd, wr) = socket.into_split();
        let transport = LineTransport::new(BufReader::new(rd), wr, DefaultCodec::default());
        let service = make_service();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = turbomcp_service::serve_with(transport, service, config).await {
                tracing::debug!(%peer, error = %e, "tcp connection ended with error");
            }
        });
    }
}

/// Serve line-framed JSON-RPC on a Unix domain socket listener with the
/// default [`ServeConfig`], one service per connection (see [`serve_tcp`]).
///
/// # Errors
/// Returns only if accepting a connection fails.
#[cfg(unix)]
pub async fn serve_unix<S, F>(listener: UnixListener, make_service: F) -> Result<(), StdioError>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: McpService + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    serve_unix_with(listener, make_service, ServeConfig::default()).await
}

/// [`serve_unix`] with an explicit [`ServeConfig`] applied to each connection;
/// its shutdown token also stops the accept loop.
///
/// # Errors
/// Returns only if accepting a connection fails.
#[cfg(unix)]
pub async fn serve_unix_with<S, F>(
    listener: UnixListener,
    make_service: F,
    config: ServeConfig,
) -> Result<(), StdioError>
where
    F: Fn() -> S + Send + Sync + 'static,
    S: McpService + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    let shutdown = config.shutdown.clone();
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted,
        };
        let (socket, _addr) = accepted?;
        let (rd, wr) = socket.into_split();
        let transport = LineTransport::new(BufReader::new(rd), wr, DefaultCodec::default());
        let service = make_service();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = turbomcp_service::serve_with(transport, service, config).await {
                tracing::debug!(error = %e, "unix connection ended with error");
            }
        });
    }
}

/// Connect to a line-framed TCP MCP server — the client-side counterpart of
/// [`serve_tcp`]. The returned transport plugs into the typed
/// `turbomcp-client` or a raw serve loop.
///
/// # Errors
/// Fails if the connection cannot be established.
pub async fn connect_tcp(
    addr: impl tokio::net::ToSocketAddrs,
) -> Result<
    LineTransport<
        BufReader<tokio::net::tcp::OwnedReadHalf>,
        tokio::net::tcp::OwnedWriteHalf,
        DefaultCodec,
    >,
    StdioError,
> {
    let socket = tokio::net::TcpStream::connect(addr).await?;
    let (rd, wr) = socket.into_split();
    Ok(LineTransport::new(
        BufReader::new(rd),
        wr,
        DefaultCodec::default(),
    ))
}

/// Connect to a line-framed Unix-socket MCP server — the client-side
/// counterpart of [`serve_unix`].
///
/// # Errors
/// Fails if the connection cannot be established.
#[cfg(unix)]
pub async fn connect_unix(
    path: impl AsRef<std::path::Path>,
) -> Result<
    LineTransport<
        BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
        DefaultCodec,
    >,
    StdioError,
> {
    let socket = tokio::net::UnixStream::connect(path).await?;
    let (rd, wr) = socket.into_split();
    Ok(LineTransport::new(
        BufReader::new(rd),
        wr,
        DefaultCodec::default(),
    ))
}
