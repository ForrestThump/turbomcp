//! The [`Extension`] seam: a multi-method server plugin (PLAN D10).
//!
//! An extension owns a set of request methods (e.g. the draft Tasks extension's
//! `tasks/get`/`tasks/update`/`tasks/cancel`), advertises itself in
//! `server/discover` under `capabilities.extensions[id]`, and is dispatched by
//! the [`VersionDispatcher`](crate::VersionDispatcher) on the modern
//! (`DRAFT-2026-v1`) path once the client has declared the extension in its
//! per-request capabilities.
//!
//! Extensions are **draft-only**: the legacy `2025-11-25` path serves its
//! built-in equivalents (core Tasks) instead. The trait is object-safe and
//! dispatched behind `Arc<dyn Extension>`, so extensions live in their own
//! crates (e.g. `turbomcp4-ext-tasks`) and register via
//! [`ServerBuilder::with_extension`](crate::ServerBuilder::with_extension) /
//! [`VersionDispatcher::with_extension`](crate::VersionDispatcher::with_extension).
//!
//! The trait is the durable architectural asset; the D10 sketch's
//! `intercept_response`/`notification_topics` are folded into the real seams an
//! extension actually needs — [`dispatch`](Extension::dispatch) for its owned
//! methods, and (Phase 9b) a task-augmentation hook for `tools/call` — rather
//! than modeled as standalone trait methods with no consumer.

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::Value;
use turbomcp4_core::{
    CancellationToken, JsonRpcError, JsonRpcMessage, JsonRpcRequest, RequestContext,
};

/// One inbound request routed to an [`Extension`]. The dispatcher has already
/// version-gated it to the modern path and verified the client declared the
/// extension capability, so a handler can trust both.
#[non_exhaustive]
pub struct ExtensionRequest {
    /// The raw JSON-RPC request; its method is one of [`Extension::methods`].
    pub request: JsonRpcRequest,
    /// The per-request context (version, identity, client capabilities, …).
    pub context: RequestContext,
    /// The driver-minted connection id, when the transport supplied one — the
    /// handle an extension uses to push server-initiated notifications back to
    /// this client via [`turbomcp4_service::outbound`].
    pub connection_id: Option<String>,
}

/// The underlying `tools/call`, prepared for an extension to run as a task.
///
/// The dispatcher builds the call's handler future (with the task's
/// cancellation token already wired into its context) and hands it over. An
/// extension that decides to taskify the call reads [`cancel_token`] (to drive
/// `tasks/cancel`), registers the task, and spawns [`run`] in the background;
/// the future resolves to the wire `CallToolResult` JSON on success, or the
/// JSON-RPC error the call would have answered with.
///
/// [`cancel_token`]: CallRunner::cancel_token
/// [`run`]: CallRunner::run
pub struct CallRunner {
    future: BoxFuture<'static, Result<Value, JsonRpcError>>,
    cancel: CancellationToken,
}

impl CallRunner {
    /// Wrap a prepared call future and the cancellation token wired into it.
    /// (Constructed by the dispatcher; extensions consume one via
    /// [`CallAugmentRequest`].)
    #[must_use]
    pub fn new(
        future: BoxFuture<'static, Result<Value, JsonRpcError>>,
        cancel: CancellationToken,
    ) -> Self {
        Self { future, cancel }
    }

    /// The cancellation token wired into the call — fire it from `tasks/cancel`
    /// (or a TTL purge) to ask the handler to stop.
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Drive the underlying call to completion. The result is the wire
    /// `CallToolResult` JSON (a tool-level `isError: true` is still `Ok` — that
    /// is a `completed` task, not a `failed` one) or the JSON-RPC error.
    pub async fn run(self) -> Result<Value, JsonRpcError> {
        self.future.await
    }
}

/// A `tools/call` offered to a call-augmenting [`Extension`]. The dispatcher
/// only constructs this for clients that declared the extension capability, so
/// returning a `CreateTaskResult` honors SEP-2663's "MUST NOT task a
/// non-declaring client".
#[non_exhaustive]
pub struct CallAugmentRequest {
    /// The `tools/call` request.
    pub request: JsonRpcRequest,
    /// The per-request context.
    pub context: RequestContext,
    /// The driver-minted connection id, for pushing `notifications/tasks`.
    pub connection_id: Option<String>,
    /// The prepared underlying call (spawn it if you take over the request).
    pub run: CallRunner,
}

/// A multi-method server extension (PLAN D10).
///
/// An extension bundles a cohesive feature that lives outside the core protocol
/// surface — owning its own wire types and request methods — and plugs into the
/// dispatcher without the core needing to know about it. The draft Tasks
/// extension (`io.modelcontextprotocol/tasks`, SEP-2663) is the reference
/// implementation; see `turbomcp4-ext-tasks`.
#[async_trait]
pub trait Extension: Send + Sync + 'static {
    /// The stable extension identifier, e.g. `io.modelcontextprotocol/tasks`.
    /// Used as the key under `server/discover` `capabilities.extensions` and as
    /// the per-request capability a client must declare to use the extension.
    fn id(&self) -> &'static str;

    /// The settings object advertised under `capabilities.extensions[id]`.
    /// Defaults to an empty object — "supported, with no settings".
    fn settings(&self) -> Value {
        Value::Object(serde_json::Map::new())
    }

    /// The request methods this extension owns. On the modern path the
    /// dispatcher routes these to [`dispatch`](Extension::dispatch); a client
    /// that has not declared the extension capability gets `-32601` for them
    /// (SEP-2663 capability negotiation).
    fn methods(&self) -> &'static [&'static str];

    /// Handle one of the extension's [`methods`](Extension::methods) and return
    /// the JSON-RPC response. The dispatcher guarantees the request's method is
    /// one this extension declared and that the client declared the extension
    /// capability.
    async fn dispatch(&self, request: ExtensionRequest) -> JsonRpcMessage;

    /// Whether this extension may convert a `tools/call` into a task. A cheap
    /// pre-check: the dispatcher only prepares a [`CallRunner`] (and consults
    /// [`augment_call`](Extension::augment_call)) when this returns `true`.
    /// Defaults to `false`.
    fn augments_calls(&self) -> bool {
        false
    }

    /// Offer a `tools/call` for task augmentation (SEP-2663). Return
    /// `Some(response)` to take over the request — a `CreateTaskResult` after
    /// spawning [`CallAugmentRequest::run`] in the background — or `None` to let
    /// the dispatcher run the call normally. Only invoked for clients that
    /// declared the extension capability. Defaults to `None` (never taskify).
    async fn augment_call(&self, _request: CallAugmentRequest) -> Option<JsonRpcMessage> {
        None
    }
}
