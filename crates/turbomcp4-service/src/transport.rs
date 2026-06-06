//! The [`Transport`] trait: a bidirectional [`JsonRpcMessage`] channel.
//!
//! Transports own framing (line-delimited for stdio, SSE-event-framed for HTTP)
//! and hand the codec complete frames. The serve loop drives a `Transport`
//! directly, so the trait uses return-position `impl Future` (native AFIT/RPITIT)
//! rather than boxed futures — no per-message allocation, fully monomorphized.
//!
//! Graceful shutdown (`graceful_shutdown(deadline)`) lands with the STDIO
//! writer-actor in Phase 4, where there's machinery to drain in-flight work;
//! Phase 2 keeps the surface to the minimum the serve loop needs.

use core::future::Future;

use turbomcp4_core::JsonRpcMessage;

/// A bidirectional channel for JSON-RPC frames.
///
/// `recv` returns `Ok(None)` on a clean end-of-stream (peer closed); `Err` is
/// reserved for genuine I/O failure. `close` consumes the transport.
pub trait Transport: Send + 'static {
    /// Transport-specific failure (I/O, protocol framing).
    type Error: core::error::Error + Send + Sync + 'static;

    /// Send one frame to the peer.
    fn send(&mut self, msg: JsonRpcMessage)
    -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Receive the next frame, or `None` at clean end-of-stream.
    fn recv(&mut self) -> impl Future<Output = Result<Option<JsonRpcMessage>, Self::Error>> + Send;

    /// Close the transport, flushing anything pending.
    fn close(self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
