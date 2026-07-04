//! # turbomcp-transport-ws
//!
//! A WebSocket transport for MCP: JSON-RPC frames carried one-per-WebSocket-
//! message over a bidirectional [`tokio_tungstenite`] stream. Like stdio (and
//! unlike Streamable HTTP), a WebSocket is a long-lived bidirectional channel,
//! so it plugs straight into the [`serve`](turbomcp_service::serve) driver.
//!
//! WebSocket is *not* an MCP-spec transport (the spec defines stdio + Streamable
//! HTTP); this is a convenience for deployments that already speak WS. Framing
//! (one message = one frame) is this crate's job; turning a frame into a value
//! is the [`Codec`]'s. Control frames (ping/pong) are handled by the library and
//! skipped here; a `Close` frame is a clean end-of-stream.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;
use turbomcp_codec::{Codec, CodecError, DefaultCodec};
use turbomcp_core::JsonRpcMessage;
use turbomcp_service::{McpService, ServeConfig, Transport};

/// Failures from the WebSocket transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WsError {
    /// An I/O error on the underlying socket.
    #[error("websocket i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A WebSocket protocol error.
    #[error("websocket protocol error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    /// A frame could not be encoded/decoded.
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
    /// An outbound frame was not valid UTF-8 (encodings are always JSON text).
    #[error("frame was not valid UTF-8")]
    Utf8,
}

/// JSON-RPC over a WebSocket: each message is one complete frame.
///
/// Generic over the underlying byte stream `S` (a `TcpStream`, a TLS stream, an
/// in-memory duplex, …) so it is reusable and unit-testable.
pub struct WebSocketTransport<S, C = DefaultCodec> {
    stream: WebSocketStream<S>,
    codec: C,
}

impl<S, C: Codec> WebSocketTransport<S, C> {
    /// Wrap an established [`WebSocketStream`] with the given codec.
    pub fn new(stream: WebSocketStream<S>, codec: C) -> Self {
        Self { stream, codec }
    }
}

impl<S> WebSocketTransport<S, DefaultCodec>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Complete the server side of the WebSocket handshake over an accepted
    /// socket, then wrap it with the [`DefaultCodec`].
    ///
    /// # Errors
    /// Fails if the handshake does not complete.
    pub async fn accept(socket: S) -> Result<Self, WsError> {
        let stream = tokio_tungstenite::accept_async(socket).await?;
        Ok(Self::new(stream, DefaultCodec::default()))
    }

    /// Adopt a raw byte stream as a WebSocket endpoint in `role` **without** a
    /// handshake — for tests over an in-memory duplex, or when the handshake was
    /// performed elsewhere.
    pub async fn from_raw(socket: S, role: Role) -> Self {
        let stream = WebSocketStream::from_raw_socket(socket, role, None).await;
        Self::new(stream, DefaultCodec::default())
    }
}

impl<S, C> Transport for WebSocketTransport<S, C>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: Codec,
{
    type Error = WsError;

    async fn send(&mut self, msg: JsonRpcMessage) -> Result<(), Self::Error> {
        let bytes = self.codec.encode(&msg)?;
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| WsError::Utf8)?;
        self.stream.send(Message::text(text)).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<JsonRpcMessage>, Self::Error> {
        while let Some(frame) = self.stream.next().await {
            match frame? {
                Message::Text(t) => return Ok(Some(self.codec.decode(t.as_bytes())?)),
                Message::Binary(b) => return Ok(Some(self.codec.decode(&b)?)),
                Message::Close(_) => return Ok(None),
                // Ping/Pong are answered by the library; ignore and read on.
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            }
        }
        Ok(None)
    }

    async fn close(mut self) -> Result<(), Self::Error> {
        self.stream.close(None).await?;
        Ok(())
    }
}

/// Connect to a WebSocket MCP server at `url` (`ws://…` or `wss://…`) and return
/// a ready [`WebSocketTransport`].
///
/// # Errors
/// Fails if the connection or handshake does not complete.
pub async fn connect(
    url: &str,
) -> Result<WebSocketTransport<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, WsError> {
    let (stream, _resp) = tokio_tungstenite::connect_async(url).await?;
    Ok(WebSocketTransport::new(stream, DefaultCodec::default()))
}

/// Serve `service` over WebSocket, accepting connections on `listener` and
/// driving each on its own task with the default [`ServeConfig`].
///
/// # Errors
/// Returns only if accepting a connection fails; per-connection errors are
/// logged and do not stop the accept loop.
pub async fn serve_websocket<S>(listener: TcpListener, service: S) -> Result<(), WsError>
where
    S: McpService + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    serve_websocket_with(listener, service, ServeConfig::default()).await
}

/// [`serve_websocket`] with an explicit [`ServeConfig`] applied to each
/// connection (shutdown token, drain deadline, concurrency bound).
///
/// # Errors
/// Returns only if accepting a connection fails.
pub async fn serve_websocket_with<S>(
    listener: TcpListener,
    service: S,
    config: ServeConfig,
) -> Result<(), WsError>
where
    S: McpService + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    loop {
        let (socket, peer) = listener.accept().await?;
        let service = service.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let transport = match WebSocketTransport::accept(socket).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::debug!(%peer, error = %e, "websocket handshake failed");
                    return;
                }
            };
            if let Err(e) = turbomcp_service::serve_with(transport, service, config).await {
                tracing::debug!(%peer, error = %e, "websocket connection ended with error");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turbomcp_core::{JsonRpcRequest, JsonRpcResponse, RequestId};

    #[tokio::test]
    async fn round_trips_frames_over_a_duplex() {
        // Two endpoints joined by an in-memory duplex, no TCP or handshake.
        let (a, b) = tokio::io::duplex(4096);
        let mut client = WebSocketTransport::from_raw(a, Role::Client).await;
        let mut server = WebSocketTransport::from_raw(b, Role::Server).await;

        // client → server request
        let req = JsonRpcRequest::new(1, "ping", None);
        client.send(JsonRpcMessage::Request(req)).await.unwrap();
        let got = server.recv().await.unwrap().expect("a frame");
        let JsonRpcMessage::Request(r) = got else {
            panic!("expected a request")
        };
        assert_eq!(r.method, "ping");

        // server → client response
        server
            .send(JsonRpcMessage::Response(JsonRpcResponse::success(
                RequestId::from(1),
                serde_json::json!({ "ok": true }),
            )))
            .await
            .unwrap();
        let got = client.recv().await.unwrap().expect("a frame");
        let JsonRpcMessage::Response(r) = got else {
            panic!("expected a response")
        };
        assert_eq!(r.result.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn close_frame_is_clean_eof() {
        let (a, b) = tokio::io::duplex(1024);
        let client = WebSocketTransport::from_raw(a, Role::Client).await;
        let mut server = WebSocketTransport::from_raw(b, Role::Server).await;
        client.close().await.unwrap();
        assert!(server.recv().await.unwrap().is_none());
    }
}
