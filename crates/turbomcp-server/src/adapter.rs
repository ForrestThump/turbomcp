//! [`LegacySessionAdapter`]: the per-connection session bridge for
//! single-client transports (stdio, TCP, …).
//!
//! HTTP carries the legacy session in the `Mcp-Session-Id` header, so the HTTP
//! runner does its own routing. A byte-pipe transport has no headers — the
//! *connection* is the session. This adapter wraps the dispatcher and supplies
//! what the pipe can't say in-band:
//!
//! 1. On `initialize`, a session id is minted and attached; once the inner
//!    service answers successfully, the connection is marked legacy.
//! 2. Subsequent messages that don't carry their own protocol version (a
//!    modern client states it per request) are stamped with the negotiated
//!    legacy version and the connection's session id.
//!
//! The adapter is itself an `McpService`, so it slots into `serve`/
//! `serve_stdio` wherever a bare dispatcher would.
//!
//! **Trust model:** the adapter does *not* sanitize inbound `_meta` — that is
//! the wire boundary's job (the `serve` driver and the HTTP endpoint both call
//! [`meta::sanitize_inbound`] before injecting their own internal keys, and the
//! driver's per-connection id must survive this adapter). Compose the adapter
//! under one of those boundaries, never directly against raw client input.

use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use serde_json::json;
use tower::Service;
use turbomcp_core::{JsonRpcMessage, ProtocolVersion, meta};
use turbomcp_protocol::{methods, version};
use turbomcp_service::ProtocolError;
use uuid::Uuid;

/// Wraps an inner `Service<JsonRpcMessage>` (normally the
/// [`VersionDispatcher`](crate::VersionDispatcher)) with per-connection legacy
/// session tracking. Construct one adapter per connection; clones share the
/// connection's session state.
pub struct LegacySessionAdapter<S> {
    inner: S,
    /// `Some(session_id)` once an `initialize` on this connection succeeded.
    session: Arc<Mutex<Option<String>>>,
}

impl<S> LegacySessionAdapter<S> {
    /// Wrap `inner` with fresh (not-yet-initialized) connection state.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            session: Arc::new(Mutex::new(None)),
        }
    }
}

impl<S: Clone> Clone for LegacySessionAdapter<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            session: Arc::clone(&self.session),
        }
    }
}

impl<S> Service<JsonRpcMessage> for LegacySessionAdapter<S>
where
    S: Service<JsonRpcMessage, Response = Option<JsonRpcMessage>, Error = ProtocolError>,
    S::Future: Send + 'static,
{
    type Response = Option<JsonRpcMessage>;
    type Error = ProtocolError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut msg: JsonRpcMessage) -> Self::Future {
        let is_initialize = matches!(
            &msg,
            JsonRpcMessage::Request(r) if r.method == methods::request::INITIALIZE
        );
        if is_initialize {
            // Mint the connection's session id now; commit it only once the
            // handshake actually succeeds, so a malformed initialize doesn't
            // flip the connection into legacy mode.
            let candidate = Uuid::new_v4().to_string();
            meta::set_request_meta(&mut msg, meta::internal::SESSION_ID, json!(candidate));
            let session = Arc::clone(&self.session);
            let fut = self.inner.call(msg);
            return Box::pin(async move {
                let out = fut.await?;
                if let Some(JsonRpcMessage::Response(resp)) = &out
                    && !resp.is_error()
                {
                    *session.lock().expect("session state lock poisoned") = Some(candidate);
                }
                Ok(out)
            });
        }

        let session = self
            .session
            .lock()
            .expect("session state lock poisoned")
            .clone();
        if let Some(sid) = session {
            let params = match &msg {
                JsonRpcMessage::Request(r) => r.params.as_ref(),
                JsonRpcMessage::Notification(n) => n.params.as_ref(),
                JsonRpcMessage::Response(_) => None,
            };
            // Stamp only version-less messages: a modern stateless client
            // sharing the pipe keeps working, per-request version wins.
            if version::request_protocol_version(params).is_none() {
                meta::set_request_meta(
                    &mut msg,
                    meta::keys::PROTOCOL_VERSION,
                    json!(ProtocolVersion::V2025_11_25.as_str()),
                );
                meta::set_request_meta(&mut msg, meta::internal::SESSION_ID, json!(sid));
            }
        }
        Box::pin(self.inner.call(msg))
    }
}
