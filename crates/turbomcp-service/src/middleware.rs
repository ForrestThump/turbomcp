//! Shared RPC middleware — `tower::Layer`s that wrap any [`McpService`] and
//! compose identically under every transport (stdio, HTTP, WS).
//!
//! Phase 2 ships [`TracingLayer`], which proves the seam: a transport-agnostic
//! layer over `Service<JsonRpcMessage>`. The `_meta`→`RequestContext` extraction
//! layer joins it in Phase 4 once the task-local context plumbing lands; until
//! then the `VersionDispatcher` extracts `_meta` itself.

use std::task::{Context, Poll};

use tower::{Layer, Service};
use tracing::Instrument;
use turbomcp_core::JsonRpcMessage;

use crate::ProtocolError;

/// A [`tower::Layer`] that wraps each RPC in a `tracing` span carrying the
/// method name. Cheap, allocation-free (`Instrumented<S::Future>` is named, not
/// boxed), and the first link in the shared RPC stack.
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingLayer;

impl<S> Layer<S> for TracingLayer {
    type Service = Tracing<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Tracing { inner }
    }
}

/// The service produced by [`TracingLayer`]. See that type for details.
#[derive(Debug, Clone)]
pub struct Tracing<S> {
    inner: S,
}

impl<S> Service<JsonRpcMessage> for Tracing<S>
where
    S: Service<JsonRpcMessage, Response = Option<JsonRpcMessage>, Error = ProtocolError>,
{
    type Response = Option<JsonRpcMessage>;
    type Error = ProtocolError;
    type Future = tracing::instrument::Instrumented<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: JsonRpcMessage) -> Self::Future {
        let method = req.method().unwrap_or("(response)").to_owned();
        let span = tracing::debug_span!("mcp.rpc", method = %method);
        self.inner.call(req).instrument(span)
    }
}
