//! The transport ↔ service driver loop.
//!
//! [`serve`] reads frames from a [`Transport`], runs each through an
//! [`McpService`] (the dispatcher, wrapped in whatever middleware), and writes
//! back any response. Notifications (service returns `None`) produce no write.
//!
//! Phase 2 processes one request at a time. Concurrent handling with ordered
//! writes (the STDIO writer-actor) and `poll_ready` backpressure parking land in
//! Phase 4; the loop honors `poll_ready` here so that change is transparent.

use std::future::poll_fn;

use crate::{McpService, ProtocolError, Transport};

/// Drive `service` from `transport` until the peer closes the stream.
///
/// Returns `Ok(())` on a clean end-of-stream. A transport failure becomes
/// [`ProtocolError::Transport`]; a service error propagates as-is.
///
/// # Errors
/// Propagates transport I/O failures and service-layer [`ProtocolError`]s.
pub async fn serve<T, S>(mut transport: T, mut service: S) -> Result<(), ProtocolError>
where
    T: Transport,
    S: McpService,
{
    loop {
        let frame = transport
            .recv()
            .await
            .map_err(|e| ProtocolError::Transport(e.to_string()))?;
        let Some(frame) = frame else { break };

        poll_fn(|cx| service.poll_ready(cx)).await?;
        if let Some(reply) = service.call(frame).await? {
            transport
                .send(reply)
                .await
                .map_err(|e| ProtocolError::Transport(e.to_string()))?;
        }
    }
    transport
        .close()
        .await
        .map_err(|e| ProtocolError::Transport(e.to_string()))?;
    Ok(())
}
