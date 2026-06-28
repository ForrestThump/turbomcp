//! The [`Transport`] trait: a bidirectional [`JsonRpcMessage`] channel.
//!
//! Transports own framing (line-delimited for stdio, SSE-event-framed for HTTP)
//! and hand the codec complete frames. The serve loop drives a `Transport`
//! directly, so the trait uses return-position `impl Future` (native AFIT/RPITIT)
//! rather than boxed futures — no per-message allocation, fully monomorphized.

use core::future::Future;
use std::time::Instant;

use turbomcp_core::JsonRpcMessage;

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

    /// Close the transport, having been given a `deadline` by which any drain of
    /// pending writes should complete (PLAN §4.13).
    ///
    /// The default flushes-and-closes via [`Transport::close`], ignoring the
    /// deadline — correct for transports whose `send` already flushes each frame
    /// (e.g. stdio's line writer). Transports that buffer or own a long-lived
    /// outbound stream (HTTP SSE, in Phase 6) override this to honor the bound.
    fn graceful_shutdown(
        self,
        _deadline: Instant,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        Self: Sized,
    {
        self.close()
    }
}
