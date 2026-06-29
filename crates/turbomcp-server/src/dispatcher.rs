//! [`VersionDispatcher`]: the `tower::Service` that turns a [`JsonRpcMessage`]
//! into a typed handler call and back.
//!
//! It lives here (not in `turbomcp-protocol`, as an early draft of the plan had
//! it) because it is generic over the user's [`McpServerCore`], which sits above
//! the protocol layer — putting it here keeps the dependency graph acyclic while
//! concentrating *all* per-version branching in one place. Above it (RPC
//! middleware) and below it (typed handlers) are version-agnostic.
//!
//! Per-version status (Phase 5): both paths are live. The modern
//! `2026-07-28` path is stateless (version in each request's `_meta`); the
//! legacy `2025-11-25` path is stateful — `initialize` negotiates a version and
//! mints a session (via the transport-supplied internal session id, see
//! [`turbomcp_core::meta::internal`]), and later requests are dispatched with
//! the session's negotiated client info/capabilities injected into their
//! [`RequestContext`]. Both paths converge on the same neutral handlers; only
//! the wire types differ (selected via the private `WireFamily` trait).
//!
//! `_meta`→context extraction may still move to a `MetaExtractLayer` once
//! Auth/RateLimit need to observe it between layers (Phase 6/7).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value};
use tower::Service;

use turbomcp_core::{
    CancellationToken, Implementation, JsonRpcError, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, McpError, ProtocolVersion, RequestContext, RequestId, meta,
};
use turbomcp_protocol::v2025_11_25::types as legacy;
use turbomcp_protocol::v2026_draft::types as draft;
use turbomcp_protocol::{methods, neutral, version};
use turbomcp_service::{ProtocolError, mcp_to_jsonrpc_error};

use crate::context::{
    CallToolContext, CompleteContext, GetPromptContext, ListPromptsContext,
    ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, ReadResourceContext,
};
use crate::extension::{
    CallAugmentRequest, CallRunner, Extension, ExtensionRequest, SubscribeOutcome,
};
use crate::inflight::InFlightRegistry;
use crate::logging::LogSender;
use crate::mrtr::{ClientHandle, PendingRequests, StateSigner};
use crate::progress::ProgressReporter;
use crate::router::MethodRouter;
use crate::session::{SessionState, SessionStore};
use crate::subscriptions::{ServerNotifier, SubscriptionRegistry, subscription_id_value};
use crate::tasks::{TaskError, TaskSnapshot, TaskStatus, TaskStore};
use crate::traits::McpServerCore;

/// The protocol seam for a server: `Service<JsonRpcMessage>`.
///
/// Clone is cheap (the server clones per request; the router is shared behind an
/// `Arc`), so the dispatcher composes under per-connection `tower` stacks.
pub struct VersionDispatcher<S> {
    server: S,
    router: Arc<MethodRouter<S>>,
    supported: Vec<ProtocolVersion>,
    shared: Shared,
}

/// The dispatcher's shared per-server state — one `Arc` per store, grouped so
/// the deep handler call chain threads a single value instead of six (and so
/// cross-store coordination, like session-termination tearing down a session's
/// subscription routes, has one place to live). Cheap to clone (six `Arc`s).
#[derive(Clone)]
struct Shared {
    sessions: Arc<SessionStore>,
    tasks: Option<Arc<TaskStore>>,
    inflight: Arc<InFlightRegistry>,
    subs: Arc<SubscriptionRegistry>,
    signer: Arc<StateSigner>,
    pending: Arc<PendingRequests>,
    /// Registered draft extensions (PLAN D10), consulted for `server/discover`
    /// advertisement and modern-path method routing. One `Arc` to keep the
    /// per-request `Shared` clone cheap.
    extensions: Arc<Vec<Arc<dyn Extension>>>,
}

impl Shared {
    /// Reclaim every idle-expired session and tear down its legacy
    /// subscription routes in one place (the session store and the
    /// subscription registry don't know about each other). Called
    /// opportunistically at `initialize`, where new sessions are minted — a
    /// natural, cheap point to bound stale-session growth without a background
    /// task. A store with no idle timeout sweeps nothing.
    fn sweep_idle_sessions(&self) {
        for id in self.sessions.sweep_expired() {
            self.subs.legacy_remove(&id);
        }
    }

    /// Terminate one session: drop its state and its legacy subscription
    /// routes. Returns whether the session existed. Backs explicit `DELETE`
    /// session termination.
    fn terminate_session(&self, id: &str) -> bool {
        let existed = self.sessions.remove(id);
        self.subs.legacy_remove(id);
        existed
    }
}

/// The [`SessionTerminator`] handle returned by
/// [`VersionDispatcher::session_terminator`]: shares the dispatcher's stores so
/// `DELETE` drops the session state and its subscription routes together.
#[derive(Clone)]
pub struct DispatcherSessionTerminator {
    shared: Shared,
}

impl turbomcp_service::SessionTerminator for DispatcherSessionTerminator {
    fn terminate(&self, session_id: &str) -> bool {
        self.shared.terminate_session(session_id)
    }
}

impl<S: Clone> Clone for VersionDispatcher<S> {
    fn clone(&self) -> Self {
        Self {
            server: self.server.clone(),
            router: Arc::clone(&self.router),
            supported: self.supported.clone(),
            shared: self.shared.clone(),
        }
    }
}

impl<S: McpServerCore> VersionDispatcher<S> {
    /// Build a dispatcher for `server` with `router`'s registered capabilities.
    /// The accepted version set is taken from [`McpServerCore::supported_versions`].
    #[must_use]
    pub fn new(server: S, router: MethodRouter<S>) -> Self {
        let supported = server.supported_versions().to_vec();
        Self {
            server,
            router: Arc::new(router),
            supported,
            shared: Shared {
                sessions: Arc::new(SessionStore::default()),
                tasks: None,
                inflight: Arc::new(InFlightRegistry::default()),
                subs: Arc::new(SubscriptionRegistry::default()),
                signer: Arc::new(StateSigner::new()),
                pending: Arc::new(PendingRequests::default()),
                extensions: Arc::new(Vec::new()),
            },
        }
    }

    /// A cloneable handle for publishing change notifications
    /// (`*_list_changed`, `resources/updated`) to every live subscription.
    #[must_use]
    pub fn notifier(&self) -> ServerNotifier {
        ServerNotifier::new(Arc::clone(&self.shared.subs))
    }

    /// A [`SessionTerminator`](turbomcp_service::SessionTerminator) handle for
    /// the HTTP transport: hand it to `HttpConfig::with_session_terminator` so a
    /// client `DELETE` ends its `2025-11-25` session (dropping the session state
    /// and its subscription routes). Without it, `DELETE` answers `405` (the
    /// spec permits refusing).
    #[must_use]
    pub fn session_terminator(&self) -> DispatcherSessionTerminator {
        DispatcherSessionTerminator {
            shared: self.shared.clone(),
        }
    }

    /// Enable core Tasks (`2025-11-25`): task-augmented `tools/call` plus
    /// `tasks/list|get|cancel|result`, advertised via the `tasks` capability
    /// at `initialize`. Tools then default to `execution.taskSupport:
    /// "optional"` in `tools/list`.
    ///
    /// Task-augmented calls run on spawned tasks, so the dispatcher must be
    /// driven inside a tokio runtime (all bundled transports do this).
    #[must_use]
    pub fn with_task_support(mut self) -> Self {
        self.shared.tasks = Some(Arc::new(TaskStore::default()));
        self
    }

    /// Register a draft [`Extension`] (PLAN D10): it is advertised in
    /// `server/discover` under `capabilities.extensions[id]` and owns its
    /// declared methods on the modern (`2026-07-28`) path. Extensions are
    /// draft-only — the legacy `2025-11-25` path serves its built-in
    /// equivalents (core Tasks via [`with_task_support`](Self::with_task_support)).
    #[must_use]
    pub fn with_extension(mut self, extension: Arc<dyn Extension>) -> Self {
        // `with_extension` runs at build time (rare); rebuild the shared `Arc`
        // so per-request `Shared` clones stay a single `Arc` bump.
        let mut extensions = Vec::clone(&self.shared.extensions);
        extensions.push(extension);
        self.shared.extensions = Arc::new(extensions);
        self
    }

    /// Evict a legacy session not seen within `timeout` (and tear down its
    /// subscription routes). Without this, sessions are bounded only by the
    /// store's LRU capacity. Call at build time, before serving.
    #[must_use]
    pub fn with_session_idle_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.shared.sessions = Arc::new(
            SessionStore::with_capacity(SessionStore::DEFAULT_CAPACITY)
                .with_idle_timeout(Some(timeout)),
        );
        self
    }
}

impl<S: McpServerCore> Service<JsonRpcMessage> for VersionDispatcher<S> {
    type Response = Option<JsonRpcMessage>;
    type Error = ProtocolError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Backpressure (bounded request queue) lands with the Phase 4
        // writer-actor; today the dispatcher is always ready.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, msg: JsonRpcMessage) -> Self::Future {
        let server = self.server.clone();
        let router = Arc::clone(&self.router);
        let supported = self.supported.clone();
        let shared = self.shared.clone();
        Box::pin(async move { handle(server, router, supported, shared, msg).await })
    }
}

async fn handle<S: McpServerCore>(
    server: S,
    router: Arc<MethodRouter<S>>,
    supported: Vec<ProtocolVersion>,
    shared: Shared,
    msg: JsonRpcMessage,
) -> Result<Option<JsonRpcMessage>, ProtocolError> {
    match msg {
        JsonRpcMessage::Request(req) => {
            // Track the request for `notifications/cancelled` while it
            // dispatches — but only on an identified connection (the serve
            // driver injects the id; HTTP cancels by closing the stream).
            let cancel = CancellationToken::new();
            let _guard = connection_id(req.params.as_ref())
                .map(|conn| shared.inflight.register(conn, &req.id, cancel.clone()));

            // `subscriptions/listen` is the one MCP request with no JSON-RPC
            // response: its stream begins with an acknowledged *notification*
            // via the connection's writer, so it can't share `handle_request`'s
            // always-respond contract.
            if req.method == methods::request::SUBSCRIPTIONS_LISTEN {
                return handle_subscriptions_listen(
                    &router,
                    &supported,
                    &shared.subs,
                    &shared.extensions,
                    &req,
                    &cancel,
                )
                .await;
            }

            let dispatch =
                handle_request(server, &router, &supported, &shared, req, cancel.clone());
            tokio::select! {
                // Cancelled mid-flight: drop the handler future and send
                // nothing (cancellation spec: "stop processing … not send a
                // response for the cancelled request").
                () = cancel.cancelled() => Ok(None),
                out = dispatch => out.map(Some),
            }
        }
        JsonRpcMessage::Notification(n) => {
            handle_notification(&shared.inflight, &shared.subs, &n);
            Ok(None)
        }
        JsonRpcMessage::Response(resp) => {
            // A client→server response answers a server-initiated inline bidi
            // request (legacy elicitation/sampling/roots): route it to the
            // awaiting handler. Unsolicited responses are ignored.
            if !shared.pending.complete(resp) {
                tracing::debug!("ignoring unsolicited client->server response");
            }
            Ok(None)
        }
    }
}

#[derive(Deserialize)]
struct RawCancelledParams {
    #[serde(rename = "requestId")]
    request_id: RequestId,
    #[serde(default)]
    reason: Option<String>,
}

fn handle_notification(
    inflight: &InFlightRegistry,
    subs: &SubscriptionRegistry,
    n: &JsonRpcNotification,
) {
    match n.method.as_str() {
        methods::notification::CANCELLED => {
            // Fire-and-forget per spec: malformed params, unknown ids, and
            // already-finished requests are all silently ignored.
            let Some(conn) = connection_id(n.params.as_ref()) else {
                tracing::debug!("notifications/cancelled without a connection; ignored");
                return;
            };
            let Some(parsed) = n
                .params
                .as_ref()
                .and_then(|p| serde_json::from_value::<RawCancelledParams>(p.clone()).ok())
            else {
                tracing::debug!("malformed notifications/cancelled; ignored");
                return;
            };
            // The id may name an in-flight request *or* a live subscription
            // (cancelling the `subscriptions/listen` request id is how a
            // stdio client closes its stream).
            let fired = inflight.cancel(conn, &parsed.request_id);
            let unsubscribed = subs.remove(conn, &parsed.request_id);
            tracing::debug!(
                request_id = ?parsed.request_id,
                reason = parsed.reason.as_deref().unwrap_or(""),
                fired,
                unsubscribed,
                "notifications/cancelled"
            );
        }
        methods::notification::INITIALIZED => {
            tracing::debug!("received notifications/initialized");
        }
        other => tracing::debug!(method = other, "unhandled notification"),
    }
}

async fn handle_request<S: McpServerCore>(
    server: S,
    router: &MethodRouter<S>,
    supported: &[ProtocolVersion],
    shared: &Shared,
    req: JsonRpcRequest,
    cancel: CancellationToken,
) -> Result<JsonRpcMessage, ProtocolError> {
    // The fields this path needs; `signer`/`pending` flow on into
    // `dispatch_capability` via `shared`.
    let Shared {
        sessions,
        tasks,
        subs,
        ..
    } = shared;
    let id = req.id.clone();
    let method = req.method.clone();

    // Extension-owned methods (PLAN D10) are draft-only: on the modern path
    // route them to the registered extension once the client has declared its
    // capability; the legacy path falls through to the built-in equivalents
    // (core Tasks) handled by the arms below.
    if let Some(ext) = shared
        .extensions
        .iter()
        .find(|e| e.methods().contains(&method.as_str()))
        .cloned()
        && matches!(
            classify_version(req.params.as_ref(), supported),
            VersionRoute::Modern
        )
    {
        let ctx = build_context(&req);
        if !context_declares_extension(&ctx, ext.id()) {
            // SEP-2663: a client that didn't declare the extension capability
            // gets `-32601` for the extension's methods.
            return Ok(error_response(id, &McpError::method_not_found(method)));
        }
        let connection_id = connection_id(req.params.as_ref()).map(str::to_owned);
        return Ok(ext
            .dispatch(ExtensionRequest {
                request: req,
                context: ctx,
                connection_id,
            })
            .await);
    }

    match method.as_str() {
        // Version-agnostic methods: a client may call these before it knows
        // which version to pin (discovery) or merely to probe liveness.
        methods::request::DISCOVER => Ok(discover_response(
            id,
            &server,
            router,
            supported,
            &shared.extensions,
        )),
        methods::request::PING => Ok(JsonRpcResponse::success(id, serde_json::json!({})).into()),

        // Stateful handshake (2025-11-25 and earlier).
        methods::request::INITIALIZE => {
            // Bound stale-session growth: reclaim idle sessions (and their
            // routes) whenever a new one is minted.
            shared.sweep_idle_sessions();
            let tasks_enabled = tasks.is_some() && router.has_tools();
            let reply =
                handle_initialize(&server, router, supported, sessions, tasks_enabled, &req);
            // A successfully initialized session gets a delivery route, so
            // list_changed notifications can reach it from the start.
            if matches!(&reply, JsonRpcMessage::Response(r) if r.error.is_none())
                && let Some(sid) = session_id(req.params.as_ref())
            {
                subs.legacy_touch(sid, connection_id(req.params.as_ref()));
            }
            Ok(reply)
        }

        // Version-gated methods (every capability `*/list|read|get|call|complete`).
        methods::request::TOOLS_LIST
        | methods::request::TOOLS_CALL
        | methods::request::RESOURCES_LIST
        | methods::request::RESOURCES_TEMPLATES_LIST
        | methods::request::RESOURCES_READ
        | methods::request::PROMPTS_LIST
        | methods::request::PROMPTS_GET
        | methods::request::COMPLETION_COMPLETE => {
            match classify_version(req.params.as_ref(), supported) {
                VersionRoute::Modern => {
                    let mut ctx = build_context(&req);
                    ctx.cancellation = cancel;
                    // Draft logging opt-in: an unrecognized level rejects the
                    // request (logging spec §Error Handling).
                    match extract_log_level(req.params.as_ref()) {
                        Ok(level) => ctx.log_level = level,
                        Err(e) => return Ok(error_response(id, &e)),
                    }
                    // Draft Tasks extension (SEP-2663): a `tools/call` from a
                    // client that declared a call-augmenting extension may be
                    // converted into a task (`CreateTaskResult`) instead of
                    // running synchronously. The extension decides per call; a
                    // `None` here falls through to the normal dispatch.
                    if method == methods::request::TOOLS_CALL
                        && let Some(resp) =
                            try_augment_call(&server, router, &req, &ctx, &shared.extensions, &id)
                                .await
                    {
                        return Ok(resp);
                    }
                    Ok(
                        dispatch_capability::<S, DraftWire>(server, router, &req, &ctx, shared, id)
                            .await,
                    )
                }
                VersionRoute::Legacy => {
                    let mut ctx = match legacy_context(sessions, &req)? {
                        Ok(ctx) => ctx,
                        Err(response) => return Ok(response),
                    };
                    ctx.cancellation = cancel;
                    // Keep the session's stdio delivery route fresh for
                    // server-initiated notifications.
                    if let Some(sid) = session_id(req.params.as_ref()) {
                        subs.legacy_touch(sid, connection_id(req.params.as_ref()));
                    }
                    // Core Tasks hooks (2025-11-25 only): augmented tools/call
                    // detaches into a task; tools/list advertises taskSupport.
                    if let Some(store) = tasks {
                        if method == methods::request::TOOLS_CALL
                            && has_task_field(req.params.as_ref())
                        {
                            return Ok(task_augmented_call(server, router, store, ctx, &req, id));
                        }
                        if method == methods::request::TOOLS_LIST {
                            return Ok(legacy_list_tools_with_task_support(
                                server, router, &req, ctx, id,
                            )
                            .await);
                        }
                    }
                    Ok(
                        dispatch_capability::<S, LegacyWire>(
                            server, router, &req, &ctx, shared, id,
                        )
                        .await,
                    )
                }
                VersionRoute::Unsupported(requested) => {
                    Ok(unsupported_version(id, requested, supported))
                }
            }
        }

        // Legacy resource subscriptions (2025-11-25; the draft subscribes via
        // `subscriptions/listen` instead).
        methods::request::RESOURCES_SUBSCRIBE | methods::request::RESOURCES_UNSUBSCRIBE => {
            match classify_version(req.params.as_ref(), supported) {
                VersionRoute::Legacy => {
                    if let Err(response) = legacy_context(sessions, &req)? {
                        return Ok(response);
                    }
                    if !router.has_resources() {
                        return Ok(error_response(id, &McpError::method_not_found(method)));
                    }
                    let uri = match parse_uri_param(req.params.as_ref(), &method) {
                        Ok(uri) => uri,
                        Err(e) => return Ok(error_response(id, &e)),
                    };
                    // `legacy_context` proved the session id is present.
                    let sid = session_id(req.params.as_ref()).unwrap_or_default();
                    if method == methods::request::RESOURCES_SUBSCRIBE {
                        subs.legacy_subscribe(sid, connection_id(req.params.as_ref()), uri);
                    } else {
                        subs.legacy_unsubscribe(sid, &uri);
                    }
                    Ok(JsonRpcResponse::success(id, serde_json::json!({})).into())
                }
                VersionRoute::Modern => Ok(error_response(id, &McpError::method_not_found(method))),
                VersionRoute::Unsupported(requested) => {
                    Ok(unsupported_version(id, requested, supported))
                }
            }
        }

        // Legacy per-session log-level opt-in (2025-11-25; the draft replaced
        // the RPC with the per-request `_meta` `logLevel` key).
        methods::request::LOGGING_SET_LEVEL => {
            match classify_version(req.params.as_ref(), supported) {
                VersionRoute::Legacy => {
                    if let Err(response) = legacy_context(sessions, &req)? {
                        return Ok(response);
                    }
                    if !router.has_logging() {
                        return Ok(error_response(id, &McpError::method_not_found(method)));
                    }
                    let level = match parse_set_level_params(req.params.as_ref()) {
                        Ok(level) => level,
                        Err(e) => return Ok(error_response(id, &e)),
                    };
                    // `legacy_context` proved the session id is present.
                    let sid = session_id(req.params.as_ref()).unwrap_or_default();
                    sessions.set_log_level(sid, level);
                    Ok(JsonRpcResponse::success(id, serde_json::json!({})).into())
                }
                VersionRoute::Modern => Ok(error_response(id, &McpError::method_not_found(method))),
                VersionRoute::Unsupported(requested) => {
                    Ok(unsupported_version(id, requested, supported))
                }
            }
        }

        // Core Tasks methods (2025-11-25; the draft serves Tasks as an
        // extension instead — Phase 8).
        methods::request::TASKS_LIST
        | methods::request::TASKS_GET
        | methods::request::TASKS_CANCEL
        | methods::request::TASKS_RESULT => {
            match classify_version(req.params.as_ref(), supported) {
                VersionRoute::Legacy => {
                    // Same session gate as every other legacy method.
                    if let Err(response) = legacy_context(sessions, &req)? {
                        return Ok(response);
                    }
                    let Some(store) = tasks else {
                        return Ok(error_response(id, &McpError::method_not_found(method)));
                    };
                    // `legacy_context` proved the session id is present.
                    let sid = session_id(req.params.as_ref())
                        .unwrap_or_default()
                        .to_owned();
                    Ok(handle_tasks_method(store, &sid, method.as_str(), &req, id).await)
                }
                VersionRoute::Modern => Ok(error_response(id, &McpError::method_not_found(method))),
                VersionRoute::Unsupported(requested) => {
                    Ok(unsupported_version(id, requested, supported))
                }
            }
        }

        other => Ok(error_response(id, &McpError::method_not_found(other))),
    }
}

/// Await a registered handler's future and widen its neutral result to the
/// active wire type `W`. A `None` future means the capability isn't registered
/// (e.g. `resources/read` on a tools-only server) → `method_not_found`.
async fn finish<N, W>(
    id: RequestId,
    method: &str,
    fut: Option<BoxFuture<'static, Result<N, McpError>>>,
) -> JsonRpcMessage
where
    W: Serialize + From<N>,
{
    match fut {
        None => error_response(id, &McpError::method_not_found(method)),
        Some(f) => match f.await {
            Ok(result) => ok_value(id, &W::from(result)),
            Err(e) => error_response(id, &e),
        },
    }
}

/// The per-version wire surface: one associated type per capability result.
/// Both versions dispatch through the same generic path; only the
/// `From<neutral>` target differs (the conversions live in
/// `turbomcp_protocol::neutral`).
trait WireFamily {
    /// Whether this wire family delivers client interaction via MRTR
    /// (`InputRequiredResult`); the legacy family uses inline bidi instead.
    const MRTR: bool;
    type ListTools: Serialize + From<neutral::ListToolsResult>;
    type CallTool: Serialize + From<neutral::CallToolResult>;
    type ListResources: Serialize + From<neutral::ListResourcesResult>;
    type ListResourceTemplates: Serialize + From<neutral::ListResourceTemplatesResult>;
    type ReadResource: Serialize + From<neutral::ReadResourceResult>;
    type ListPrompts: Serialize + From<neutral::ListPromptsResult>;
    type GetPrompt: Serialize + From<neutral::GetPromptResult>;
    type Complete: Serialize + From<neutral::CompleteResult>;
}

/// `2026-07-28` (modern, stateless).
struct DraftWire;

impl WireFamily for DraftWire {
    const MRTR: bool = true;
    type ListTools = draft::ListToolsResult;
    type CallTool = draft::CallToolResult;
    type ListResources = draft::ListResourcesResult;
    type ListResourceTemplates = draft::ListResourceTemplatesResult;
    type ReadResource = draft::ReadResourceResult;
    type ListPrompts = draft::ListPromptsResult;
    type GetPrompt = draft::GetPromptResult;
    type Complete = draft::CompleteResult;
}

/// `2025-11-25` (legacy, stateful).
struct LegacyWire;

impl WireFamily for LegacyWire {
    const MRTR: bool = false;
    type ListTools = legacy::ListToolsResult;
    type CallTool = legacy::CallToolResult;
    type ListResources = legacy::ListResourcesResult;
    type ListResourceTemplates = legacy::ListResourceTemplatesResult;
    type ReadResource = legacy::ReadResourceResult;
    type ListPrompts = legacy::ListPromptsResult;
    type GetPrompt = legacy::GetPromptResult;
    type Complete = legacy::CompleteResult;
}

async fn dispatch_capability<S: McpServerCore, W: WireFamily>(
    server: S,
    router: &MethodRouter<S>,
    req: &JsonRpcRequest,
    ctx: &RequestContext,
    shared: &Shared,
    id: RequestId,
) -> JsonRpcMessage {
    let signer = &shared.signer;
    let pending = &shared.pending;
    let method = req.method.as_str();
    let ctx = ctx.clone();
    let list_params = parse_list_params(req.params.as_ref());
    match method {
        methods::request::TOOLS_LIST => {
            let fut = router.dispatch_list_tools(server, ListToolsContext::new(ctx), list_params);
            finish::<_, W::ListTools>(id, method, fut).await
        }
        methods::request::TOOLS_CALL => {
            let params = match parse_call_tool_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let handle = match mrtr_handle::<W>(req, &ctx, signer, pending) {
                Ok(h) => h,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_call_tool(
                server,
                CallToolContext::new(ctx.clone())
                    .with_client(handle.clone())
                    .with_progress(progress_reporter::<W>(req))
                    .with_log(log_sender::<W>(req, &ctx, router.has_logging())),
                params,
            );
            let subject = ctx.identity.subject().map(str::to_owned);
            finish_mrtr::<_, W::CallTool>(id, method, subject, fut, &handle, signer, W::MRTR).await
        }
        methods::request::RESOURCES_LIST => {
            let fut =
                router.dispatch_list_resources(server, ListResourcesContext::new(ctx), list_params);
            finish::<_, W::ListResources>(id, method, fut).await
        }
        methods::request::RESOURCES_TEMPLATES_LIST => {
            let fut = router.dispatch_list_resource_templates(
                server,
                ListResourceTemplatesContext::new(ctx),
                list_params,
            );
            finish::<_, W::ListResourceTemplates>(id, method, fut).await
        }
        methods::request::RESOURCES_READ => {
            let params = match parse_read_resource_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let handle = match mrtr_handle::<W>(req, &ctx, signer, pending) {
                Ok(h) => h,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_read_resource(
                server,
                ReadResourceContext::new(ctx.clone())
                    .with_client(handle.clone())
                    .with_progress(progress_reporter::<W>(req))
                    .with_log(log_sender::<W>(req, &ctx, router.has_logging())),
                params,
            );
            let subject = ctx.identity.subject().map(str::to_owned);
            finish_mrtr::<_, W::ReadResource>(id, method, subject, fut, &handle, signer, W::MRTR)
                .await
        }
        methods::request::PROMPTS_LIST => {
            let fut =
                router.dispatch_list_prompts(server, ListPromptsContext::new(ctx), list_params);
            finish::<_, W::ListPrompts>(id, method, fut).await
        }
        methods::request::PROMPTS_GET => {
            let params = match parse_get_prompt_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let handle = match mrtr_handle::<W>(req, &ctx, signer, pending) {
                Ok(h) => h,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_get_prompt(
                server,
                GetPromptContext::new(ctx.clone())
                    .with_client(handle.clone())
                    .with_progress(progress_reporter::<W>(req))
                    .with_log(log_sender::<W>(req, &ctx, router.has_logging())),
                params,
            );
            let subject = ctx.identity.subject().map(str::to_owned);
            finish_mrtr::<_, W::GetPrompt>(id, method, subject, fut, &handle, signer, W::MRTR).await
        }
        methods::request::COMPLETION_COMPLETE => {
            let params = match parse_complete_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_complete(server, CompleteContext::new(ctx), params);
            finish::<_, W::Complete>(id, method, fut).await
        }
        _ => unreachable!("dispatch_capability called with an unrouted method"),
    }
}

// ---- draft Tasks extension augmentation (SEP-2663) -----------------------------

/// Offer a draft `tools/call` to each call-augmenting extension the client
/// declared. The first extension to take over returns the response (a
/// `CreateTaskResult`); `None` means run the call normally. Only declared
/// clients are offered augmentation — SEP-2663 forbids returning a
/// `CreateTaskResult` to a client that didn't declare the extension.
async fn try_augment_call<S: McpServerCore>(
    server: &S,
    router: &MethodRouter<S>,
    req: &JsonRpcRequest,
    ctx: &RequestContext,
    extensions: &[Arc<dyn Extension>],
    id: &RequestId,
) -> Option<JsonRpcMessage> {
    for ext in extensions {
        if !(ext.augments_calls() && context_declares_extension(ctx, ext.id())) {
            continue;
        }
        let run = match build_call_runner(server, router, req, ctx) {
            Ok(run) => run,
            // A malformed `tools/call` envelope is `-32602` regardless of
            // augmentation (mirrors the normal `dispatch_capability` path).
            Err(e) => return Some(error_response(id.clone(), &e)),
        };
        let connection_id = connection_id(req.params.as_ref()).map(str::to_owned);
        if let Some(resp) = ext
            .augment_call(CallAugmentRequest {
                request: req.clone(),
                context: ctx.clone(),
                connection_id,
                run,
            })
            .await
        {
            return Some(resp);
        }
    }
    None
}

/// Prepare the underlying `tools/call` as a [`CallRunner`]: parse the envelope,
/// mint the task's cancellation token, wire it into a fresh context, and build
/// the handler future that renders to draft `CallToolResult` JSON (or the
/// JSON-RPC error). Taskified calls get no MRTR/progress/log channel — the
/// originating request returns `CreateTaskResult` immediately, so its stream is
/// gone; mid-task input rides `tasks/get`/`tasks/update` instead (9c).
fn build_call_runner<S: McpServerCore>(
    server: &S,
    router: &MethodRouter<S>,
    req: &JsonRpcRequest,
    ctx: &RequestContext,
) -> Result<CallRunner, McpError> {
    let params = parse_call_tool_params(req.params.as_ref())?;
    let cancel = CancellationToken::new();
    let mut call_ctx = ctx.clone();
    call_ctx.cancellation = cancel.clone();
    let fut = router.dispatch_call_tool(server.clone(), CallToolContext::new(call_ctx), params);
    let future: BoxFuture<'static, Result<Value, JsonRpcError>> = Box::pin(async move {
        match fut {
            None => Err(mcp_to_jsonrpc_error(&McpError::method_not_found(
                methods::request::TOOLS_CALL,
            ))),
            Some(f) => match f.await {
                Ok(result) => serde_json::to_value(draft::CallToolResult::from(result))
                    .map_err(|e| mcp_to_jsonrpc_error(&McpError::internal(e.to_string()))),
                Err(e) => Err(mcp_to_jsonrpc_error(&e)),
            },
        }
    });
    Ok(CallRunner::new(future, cancel))
}

// ---- MRTR (SEP-2322) -----------------------------------------------------------

#[derive(Deserialize, Default)]
struct RawMrtrFields {
    #[serde(rename = "inputResponses", default)]
    input_responses: Option<BTreeMap<String, Value>>,
    #[serde(rename = "requestState", default)]
    request_state: Option<String>,
}

/// Build the request's [`ClientHandle`]: on the draft, an MRTR coordinator
/// seeded with the retry's `inputResponses` and verified `requestState`
/// (verification failure rejects the request before the handler runs — the
/// blob is attacker-controlled); on the legacy family, an inline-bidi handle
/// bound to the request's session.
fn mrtr_handle<W: WireFamily>(
    req: &JsonRpcRequest,
    ctx: &RequestContext,
    signer: &StateSigner,
    pending: &Arc<PendingRequests>,
) -> Result<ClientHandle, McpError> {
    if !W::MRTR {
        // The legacy session gate ran before dispatch, so the session id is
        // present on this path; its absence means no client channel.
        return Ok(match session_id(req.params.as_ref()) {
            Some(session) => ClientHandle::bidi(
                session,
                connection_id(req.params.as_ref()).unwrap_or_default(),
                Arc::clone(pending),
                ctx.client_capabilities.clone(),
            ),
            None => ClientHandle::unavailable("no session for inline bidirectional requests"),
        });
    }
    let fields: RawMrtrFields = req
        .params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
        .unwrap_or_default();
    let state_in = match &fields.request_state {
        Some(token) => Some(signer.verify(&req.method, ctx.identity.subject(), token)?),
        None => None,
    };
    Ok(ClientHandle::mrtr(
        ctx.client_capabilities.clone(),
        fields.input_responses.unwrap_or_default(),
        state_in,
    ))
}

/// [`finish`], plus MRTR-abort interception: when the handler bailed with the
/// [`McpError::InputRequired`] sentinel on an MRTR-capable wire, answer an
/// `InputRequiredResult` carrying the recorded input requests and the signed
/// outbound `requestState` (the spec's MUST: at least one of the two).
async fn finish_mrtr<N, WIRE>(
    id: RequestId,
    method: &str,
    subject: Option<String>,
    fut: Option<BoxFuture<'static, Result<N, McpError>>>,
    handle: &ClientHandle,
    signer: &StateSigner,
    mrtr_enabled: bool,
) -> JsonRpcMessage
where
    WIRE: Serialize + From<N>,
{
    let Some(f) = fut else {
        return error_response(id, &McpError::method_not_found(method));
    };
    match f.await {
        Ok(result) => ok_value(id, &WIRE::from(result)),
        Err(McpError::InputRequired) if mrtr_enabled => {
            let collected = handle.collected();
            let state_out = handle.state_out();
            if collected.is_empty() && state_out.is_none() {
                // The spec requires at least one of inputRequests/requestState;
                // a bare sentinel means a handler leaked it manually.
                return error_response(
                    id,
                    &McpError::internal("MRTR abort recorded no input requests"),
                );
            }
            let mut result = Map::new();
            result.insert(
                "resultType".to_owned(),
                serde_json::json!(neutral::result_type::INPUT_REQUIRED),
            );
            if !collected.is_empty() {
                result.insert(
                    "inputRequests".to_owned(),
                    Value::Object(collected.into_iter().collect()),
                );
            }
            if let Some(data) = state_out {
                match signer.sign(method, subject.as_deref(), &data) {
                    Ok(token) => {
                        result.insert("requestState".to_owned(), serde_json::json!(token));
                    }
                    Err(e) => return error_response(id, &e),
                }
            }
            JsonRpcResponse::success(id, Value::Object(result)).into()
        }
        Err(e) => error_response(id, &e),
    }
}

// ---- version routing ---------------------------------------------------------

enum VersionRoute {
    Modern,
    Legacy,
    /// Requested version (or `None` if absent) is not supported.
    Unsupported(Option<String>),
}

fn classify_version(params: Option<&Value>, supported: &[ProtocolVersion]) -> VersionRoute {
    match version::request_protocol_version(params) {
        Some(v) if !supported.contains(&v) => {
            VersionRoute::Unsupported(Some(v.as_str().to_owned()))
        }
        Some(ProtocolVersion::V2025_11_25) => VersionRoute::Legacy,
        Some(_) => VersionRoute::Modern,
        // Missing version on a stateless (modern) method: per PLAN §4.9 we treat
        // absence as unsupported and return the server's version list, so a
        // client that omitted `_meta` can re-issue with a known version.
        None => VersionRoute::Unsupported(None),
    }
}

// ---- subscriptions (draft `subscriptions/listen`) ------------------------------

#[derive(Deserialize)]
struct RawListenParams {
    notifications: draft::SubscriptionFilter,
}

/// Open a subscription stream (subscriptions spec): validate the filter,
/// intersect it with the capabilities this server actually registered, push
/// `notifications/subscriptions/acknowledged` as the stream's first message,
/// and commit the subscription. Success returns `Ok(None)` — the listen
/// request never gets a JSON-RPC response; only failures answer in-band.
async fn handle_subscriptions_listen<S: McpServerCore>(
    router: &MethodRouter<S>,
    supported: &[ProtocolVersion],
    subs: &Arc<SubscriptionRegistry>,
    extensions: &[Arc<dyn Extension>],
    req: &JsonRpcRequest,
    cancel: &CancellationToken,
) -> Result<Option<JsonRpcMessage>, ProtocolError> {
    let id = req.id.clone();
    match classify_version(req.params.as_ref(), supported) {
        VersionRoute::Modern => {}
        // The legacy path subscribes via `resources/subscribe` instead.
        VersionRoute::Legacy => {
            return Ok(Some(error_response(
                id,
                &McpError::method_not_found(methods::request::SUBSCRIPTIONS_LISTEN),
            )));
        }
        VersionRoute::Unsupported(requested) => {
            return Ok(Some(unsupported_version(id, requested, supported)));
        }
    }

    // Streaming needs an ordered writer for this connection (the serve driver
    // registers one; the HTTP endpoint registers a per-stream one).
    let writer = connection_id(req.params.as_ref()).and_then(turbomcp_service::outbound::writer);
    let Some(writer) = writer else {
        let err = JsonRpcError {
            code: -32600,
            message: "subscriptions/listen requires a connection that can stream notifications"
                .to_owned(),
            data: None,
        };
        return Ok(Some(JsonRpcResponse::error(id, err).into()));
    };
    // `writer` resolving proves the id exists; keep it for the registry key.
    let conn = connection_id(req.params.as_ref())
        .unwrap_or_default()
        .to_owned();

    let requested: RawListenParams = match req
        .params
        .as_ref()
        .map(|p| serde_json::from_value(p.clone()))
    {
        Some(Ok(p)) => p,
        _ => {
            return Ok(Some(error_response(
                id,
                &McpError::invalid_params("subscriptions/listen requires a `notifications` filter"),
            )));
        }
    };

    // Honor only what the server can actually emit; unsupported types are
    // omitted from the acknowledgment (spec §Acknowledgment).
    let wanted = requested.notifications;
    let agreed = draft::SubscriptionFilter {
        tools_list_changed: (wanted.tools_list_changed == Some(true) && router.has_tools())
            .then_some(true),
        resources_list_changed: (wanted.resources_list_changed == Some(true)
            && router.has_resources())
        .then_some(true),
        prompts_list_changed: (wanted.prompts_list_changed == Some(true) && router.has_prompts())
            .then_some(true),
        resource_subscriptions: if router.has_resources() {
            wanted.resource_subscriptions
        } else {
            Vec::new()
        },
    };

    // The acknowledgment's `notifications` echoes the core filters the server
    // agreed to honor, plus any extension-owned filters (e.g. the Tasks
    // extension's `taskIds`). Build it as a value so extensions can merge in.
    let mut ack_notifications =
        serde_json::to_value(&agreed).unwrap_or_else(|_| Value::Object(Map::new()));
    // Offer the raw `notifications` filter to each extension (it reads its own
    // fields). A non-declaring client requesting an extension's notifications
    // is `-32003` (SEP-2663); accepted filters are merged into the ack.
    if !extensions.is_empty() {
        let raw_notifications = req
            .params
            .as_ref()
            .and_then(|p| p.get("notifications"))
            .cloned()
            .unwrap_or(Value::Null);
        let ctx = build_context(req);
        for ext in extensions {
            let declared = context_declares_extension(&ctx, ext.id());
            match ext.on_subscribe(&conn, &raw_notifications, declared) {
                SubscribeOutcome::NotApplicable => {}
                SubscribeOutcome::MissingCapability => {
                    return Ok(Some(missing_capability_response(id, ext.id())));
                }
                SubscribeOutcome::Subscribed(contribution) => {
                    if let (Some(ack_obj), Some(extra)) =
                        (ack_notifications.as_object_mut(), contribution.as_object())
                    {
                        for (key, value) in extra {
                            ack_obj.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }
    }

    // Acknowledged MUST be the first message on the stream — send it before
    // the subscription can receive its first event.
    let ack = JsonRpcNotification::new(
        methods::notification::SUBSCRIPTIONS_ACKNOWLEDGED,
        Some(serde_json::json!({
            "_meta": { meta::keys::SUBSCRIPTION_ID: subscription_id_value(&id) },
            "notifications": ack_notifications,
        })),
    );
    if writer.send(ack.into()).await.is_err() {
        return Ok(None); // connection already gone; nothing to answer
    }
    subs.insert(&conn, &id, agreed);
    // A `notifications/cancelled` that raced this dispatch fired our in-flight
    // token before the insert could be seen — honor it now.
    if cancel.is_cancelled() {
        subs.remove(&conn, &id);
    }
    Ok(None)
}

// ---- legacy (2025-11-25) session path ------------------------------------------

/// Read the transport-asserted session id from a request's `params._meta`.
/// Transports sanitize inbound messages before injecting this key, so its
/// presence is trustworthy in-process (see [`meta::internal`]).
fn session_id(params: Option<&Value>) -> Option<&str> {
    params?
        .get("_meta")?
        .get(meta::internal::SESSION_ID)?
        .as_str()
}

/// Read the driver-asserted connection id from `params._meta` (same trust
/// model as [`session_id`]: the boundary sanitizes before injecting).
fn connection_id(params: Option<&Value>) -> Option<&str> {
    params?
        .get("_meta")?
        .get(meta::internal::CONNECTION_ID)?
        .as_str()
}

/// Build the request's [`LogSender`]: live when the server enabled `logging`
/// AND the client opted in (the context's `log_level` carries the opt-in from
/// either the draft `_meta` key or the legacy session's `setLevel`). Routing
/// mirrors [`progress_reporter`].
fn log_sender<W: WireFamily>(
    req: &JsonRpcRequest,
    ctx: &RequestContext,
    logging_enabled: bool,
) -> LogSender {
    let Some(min) = ctx.log_level.filter(|_| logging_enabled) else {
        return LogSender::disabled();
    };
    let connection = connection_id(req.params.as_ref())
        .unwrap_or_default()
        .to_owned();
    let session = if W::MRTR {
        String::new()
    } else {
        session_id(req.params.as_ref())
            .unwrap_or_default()
            .to_owned()
    };
    LogSender::new(min, connection, session)
}

/// Build the request's [`ProgressReporter`]: live when the request carried a
/// `_meta.progressToken` (string or integer per the progress spec — anything
/// else is treated as absent, with a warning), inert otherwise. Notifications
/// route to the request's own stream; the legacy family may fall back to the
/// session `GET` stream, the draft never does.
fn progress_reporter<W: WireFamily>(req: &JsonRpcRequest) -> ProgressReporter {
    let token = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(meta::keys::PROGRESS_TOKEN));
    let Some(token) = token else {
        return ProgressReporter::disabled();
    };
    if !(token.is_string() || token.is_i64() || token.is_u64()) {
        tracing::warn!(?token, "progressToken must be a string or integer; ignored");
        return ProgressReporter::disabled();
    }
    let connection = connection_id(req.params.as_ref())
        .unwrap_or_default()
        .to_owned();
    let session = if W::MRTR {
        String::new()
    } else {
        session_id(req.params.as_ref())
            .unwrap_or_default()
            .to_owned()
    };
    ProgressReporter::new(token.clone(), connection, session)
}

/// Resolve the session for a legacy request and build its [`RequestContext`]
/// (negotiated version + client info/capabilities from `initialize`, plus this
/// request's propagated `_meta`).
///
/// Three-way outcome, hence the nested `Result`:
/// - `Err(ProtocolError::UnknownSession)` — id supplied but not live; the HTTP
///   transport maps this to `404` so the client re-initializes (spec §Session
///   Management).
/// - `Ok(Err(response))` — no session id at all: the connection never ran
///   `initialize`, answered in-band as a JSON-RPC error.
/// - `Ok(Ok(ctx))` — live session.
fn legacy_context(
    sessions: &SessionStore,
    req: &JsonRpcRequest,
) -> Result<Result<RequestContext, JsonRpcMessage>, ProtocolError> {
    let Some(sid) = session_id(req.params.as_ref()) else {
        let err = JsonRpcError {
            code: -32002,
            message: "server not initialized: send `initialize` first".to_owned(),
            data: None,
        };
        return Ok(Err(JsonRpcResponse::error(req.id.clone(), err).into()));
    };
    let Some(state) = sessions.get(sid) else {
        return Err(ProtocolError::UnknownSession(sid.to_owned()));
    };
    let mut ctx = RequestContext::new(state.version).with_client_info(state.client_info);
    ctx.client_capabilities = Some(state.client_capabilities);
    ctx.log_level = state.log_level;
    if let Some(m) = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(Value::as_object)
    {
        ctx.identity = meta::extract_identity(m);
        ctx.trace_context = meta::extract_trace_context(m);
        let (_consumed, propagated) = meta::partition(m.clone());
        ctx = ctx.with_propagated_meta(propagated);
    }
    Ok(Ok(ctx))
}

/// Answer the `initialize` handshake (lifecycle spec §Version Negotiation):
/// echo the requested version when supported, otherwise our latest
/// `initialize`-speaking version. When the transport supplied a session id,
/// the negotiated state is stored under it for later context injection.
fn handle_initialize<S: McpServerCore>(
    server: &S,
    router: &MethodRouter<S>,
    supported: &[ProtocolVersion],
    sessions: &SessionStore,
    tasks_enabled: bool,
    req: &JsonRpcRequest,
) -> JsonRpcMessage {
    let id = req.id.clone();
    let Some(params) = req.params.as_ref() else {
        return error_response(id, &McpError::invalid_params("initialize requires params"));
    };
    let params: legacy::InitializeRequestParams = match serde_json::from_value(params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return error_response(
                id,
                &McpError::invalid_params(format!("invalid initialize params: {e}")),
            );
        }
    };

    let negotiated = negotiate_initialize_version(&params.protocol_version, supported);

    if let Some(sid) = session_id(req.params.as_ref()) {
        let client_capabilities = serde_json::to_value(&params.capabilities).unwrap_or(Value::Null);
        sessions.insert(
            sid,
            SessionState {
                version: negotiated.clone(),
                client_info: from_legacy_impl(params.client_info),
                client_capabilities,
                log_level: None,
            },
        );
    }

    let result = legacy::InitializeResult {
        capabilities: build_legacy_capabilities(router),
        instructions: server.instructions(),
        meta: Map::new(),
        protocol_version: negotiated.as_str().to_owned(),
        server_info: to_legacy_impl(server.server_info()),
    };
    let mut value = match serde_json::to_value(&result) {
        Ok(v) => v,
        Err(e) => {
            return error_response(id, &McpError::internal(format!("serialize result: {e}")));
        }
    };
    // The generated capability types model presence-markers (`completions: {}`,
    // the `tasks` sub-objects) as maps that serde skips when empty, so an
    // advertised-but-empty marker would vanish. Patch them into the serialized
    // form instead.
    if let Some(caps) = value.get_mut("capabilities").and_then(Value::as_object_mut) {
        if router.has_completions() {
            caps.insert("completions".to_owned(), serde_json::json!({}));
        }
        if router.has_logging() {
            caps.insert("logging".to_owned(), serde_json::json!({}));
        }
        if tasks_enabled {
            caps.insert(
                "tasks".to_owned(),
                serde_json::json!({
                    "list": {},
                    "cancel": {},
                    "requests": { "tools": { "call": {} } },
                }),
            );
        }
    }
    JsonRpcResponse::success(id, value).into()
}

/// Pick the version to answer `initialize` with. The spec: echo the requested
/// version if supported, else "another version [the server] supports … SHOULD
/// be the latest". Our latest *`initialize`-speaking* version is `2025-11-25`,
/// so prefer it over the draft (which negotiates per-request instead).
fn negotiate_initialize_version(requested: &str, supported: &[ProtocolVersion]) -> ProtocolVersion {
    let requested = ProtocolVersion::from_wire(requested);
    if supported.contains(&requested) {
        return requested;
    }
    if supported.contains(&ProtocolVersion::V2025_11_25) {
        return ProtocolVersion::V2025_11_25;
    }
    supported
        .first()
        .cloned()
        .unwrap_or(ProtocolVersion::LATEST)
}

fn build_legacy_capabilities<S: McpServerCore>(
    router: &MethodRouter<S>,
) -> legacy::ServerCapabilities {
    // `listChanged`/`subscribe` are true: the subscription registry delivers
    // them for every registered capability (`resources/subscribe` + the
    // session's notification stream).
    legacy::ServerCapabilities {
        // `completions` is a presence marker the generated type can't express
        // (empty map ⇒ skipped); patched post-serialization in
        // `handle_initialize`.
        completions: Map::new(),
        experimental: BTreeMap::new(),
        logging: Map::new(),
        prompts: router
            .has_prompts()
            .then_some(legacy::ServerCapabilitiesPrompts {
                list_changed: Some(true),
            }),
        resources: router
            .has_resources()
            .then_some(legacy::ServerCapabilitiesResources {
                list_changed: Some(true),
                subscribe: Some(true),
            }),
        tasks: None,
        tools: router
            .has_tools()
            .then_some(legacy::ServerCapabilitiesTools {
                list_changed: Some(true),
            }),
    }
}

fn to_legacy_impl(i: Implementation) -> legacy::Implementation {
    legacy::Implementation {
        description: None,
        icons: Vec::new(),
        name: i.name,
        title: i.title,
        version: i.version,
        website_url: None,
    }
}

fn from_legacy_impl(i: legacy::Implementation) -> Implementation {
    let mut out = Implementation::new(i.name, i.version);
    out.title = i.title;
    out
}

// ---- core Tasks (2025-11-25) ---------------------------------------------------

/// How many tasks one `tasks/list` page carries.
const TASKS_PAGE_SIZE: usize = 50;

/// Whether a `tools/call` request asks for task-augmented execution. The
/// field's *shape* is validated in [`task_augmented_call`]; mere presence
/// routes there. (With Tasks disabled the field is ignored entirely and the
/// call processes normally, per spec §Task Support and Handling.)
fn has_task_field(params: Option<&Value>) -> bool {
    params.and_then(|p| p.get("task")).is_some()
}

#[derive(Deserialize)]
struct RawTaskMetadata {
    #[serde(default)]
    ttl: Option<i64>,
}

/// `tools/call` with a `task` field: validate, register the task, spawn the
/// handler under the task's cancellation token, and answer immediately with
/// `CreateTaskResult` (spec §Creating Tasks).
fn task_augmented_call<S: McpServerCore>(
    server: S,
    router: &MethodRouter<S>,
    store: &Arc<TaskStore>,
    ctx: RequestContext,
    req: &JsonRpcRequest,
    id: RequestId,
) -> JsonRpcMessage {
    let task_meta: RawTaskMetadata = match req
        .params
        .as_ref()
        .and_then(|p| p.get("task"))
        .map(|t| serde_json::from_value(t.clone()))
    {
        Some(Ok(m)) => m,
        _ => {
            return error_response(
                id,
                &McpError::invalid_params("invalid tools/call `task` augmentation"),
            );
        }
    };
    let params = match parse_call_tool_params(req.params.as_ref()) {
        Ok(p) => p,
        Err(e) => return error_response(id, &e),
    };

    // The task's token doubles as the handler's request cancellation, so
    // `tasks/cancel` (and ttl purge) reach a cooperative handler.
    let token = CancellationToken::new();
    let mut ctx = ctx;
    ctx.cancellation = token.clone();
    let Some(fut) = router.dispatch_call_tool(server, CallToolContext::new(ctx), params) else {
        return error_response(
            id,
            &McpError::method_not_found(methods::request::TOOLS_CALL),
        );
    };

    // The legacy gate guarantees a session id by the time we're here.
    let sid = session_id(req.params.as_ref())
        .unwrap_or_default()
        .to_owned();
    let snap = match store.create(sid, task_meta.ttl, token.clone()) {
        Ok(s) => s,
        Err(e) => return task_error_response(id, &e),
    };

    let store = Arc::clone(store);
    let task_id = snap.task_id.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = token.cancelled() => {
                // `tasks/cancel` (or expiry purge) already transitioned the
                // record; dropping `fut` aborts the handler.
            }
            out = fut => {
                let outcome = match out {
                    Ok(result) => {
                        serde_json::to_value(legacy::CallToolResult::from(result)).map_err(|e| {
                            JsonRpcError {
                                code: -32603,
                                message: format!("serialize result: {e}"),
                                data: None,
                            }
                        })
                    }
                    Err(e) => Err(mcp_to_jsonrpc_error(&e)),
                };
                store.complete(&task_id, outcome);
            }
        }
    });

    ok_value(
        id,
        &legacy::CreateTaskResult {
            meta: Map::new(),
            task: to_wire_task(&snap),
        },
    )
}

/// Legacy `tools/list` with Tasks enabled: every tool that doesn't declare its
/// own task support is advertised as `execution.taskSupport: "optional"`
/// (the conversion layer can't know Tasks are on, so the dispatcher patches).
async fn legacy_list_tools_with_task_support<S: McpServerCore>(
    server: S,
    router: &MethodRouter<S>,
    req: &JsonRpcRequest,
    ctx: RequestContext,
    id: RequestId,
) -> JsonRpcMessage {
    let list_params = parse_list_params(req.params.as_ref());
    let Some(fut) = router.dispatch_list_tools(server, ListToolsContext::new(ctx), list_params)
    else {
        return error_response(
            id,
            &McpError::method_not_found(methods::request::TOOLS_LIST),
        );
    };
    match fut.await {
        Ok(result) => {
            let mut wire = legacy::ListToolsResult::from(result);
            for tool in &mut wire.tools {
                tool.execution
                    .get_or_insert(legacy::ToolExecution { task_support: None })
                    .task_support
                    .get_or_insert(legacy::ToolExecutionTaskSupport::Optional);
            }
            ok_value(id, &wire)
        }
        Err(e) => error_response(id, &e),
    }
}

async fn handle_tasks_method(
    store: &Arc<TaskStore>,
    sid: &str,
    method: &str,
    req: &JsonRpcRequest,
    id: RequestId,
) -> JsonRpcMessage {
    match method {
        methods::request::TASKS_LIST => {
            let cursor = req
                .params
                .as_ref()
                .and_then(|p| p.get("cursor"))
                .and_then(Value::as_str);
            match store.list(sid, cursor, TASKS_PAGE_SIZE) {
                Ok((page, next_cursor)) => ok_value(
                    id,
                    &legacy::ListTasksResult {
                        meta: Map::new(),
                        next_cursor,
                        tasks: page.iter().map(to_wire_task).collect(),
                    },
                ),
                Err(e) => task_error_response(id, &e),
            }
        }
        methods::request::TASKS_GET => match parse_task_id(req.params.as_ref()) {
            Err(e) => error_response(id, &e),
            Ok(tid) => match store.get(sid, &tid) {
                Ok(s) => ok_value(
                    id,
                    &legacy::GetTaskResult {
                        created_at: s.created_at.clone(),
                        last_updated_at: s.last_updated_at.clone(),
                        meta: Map::new(),
                        poll_interval: Some(TaskStore::POLL_INTERVAL_MS),
                        status: to_wire_status(s.status),
                        status_message: s.status_message.clone(),
                        task_id: s.task_id.clone(),
                        ttl: Some(s.ttl_ms),
                        extra: Map::new(),
                    },
                ),
                Err(e) => task_error_response(id, &e),
            },
        },
        methods::request::TASKS_CANCEL => match parse_task_id(req.params.as_ref()) {
            Err(e) => error_response(id, &e),
            Ok(tid) => match store.cancel(sid, &tid) {
                Ok(s) => ok_value(
                    id,
                    &legacy::CancelTaskResult {
                        created_at: s.created_at.clone(),
                        last_updated_at: s.last_updated_at.clone(),
                        meta: Map::new(),
                        poll_interval: Some(TaskStore::POLL_INTERVAL_MS),
                        status: to_wire_status(s.status),
                        status_message: s.status_message.clone(),
                        task_id: s.task_id.clone(),
                        ttl: Some(s.ttl_ms),
                        extra: Map::new(),
                    },
                ),
                Err(e) => task_error_response(id, &e),
            },
        },
        methods::request::TASKS_RESULT => match parse_task_id(req.params.as_ref()) {
            Err(e) => error_response(id, &e),
            // Blocks until the task is terminal, then answers exactly what the
            // underlying request would have (spec §Result Retrieval).
            Ok(tid) => match store.wait_result(sid, &tid).await {
                Ok(Ok(value)) => JsonRpcResponse::success(id, value).into(),
                Ok(Err(err)) => JsonRpcResponse::error(id, err).into(),
                Err(e) => task_error_response(id, &e),
            },
        },
        _ => unreachable!("handle_tasks_method called with an unrouted method"),
    }
}

#[derive(Deserialize)]
struct RawTaskIdParams {
    #[serde(rename = "taskId")]
    task_id: String,
}

fn parse_task_id(params: Option<&Value>) -> Result<String, McpError> {
    let params = params.ok_or_else(|| McpError::invalid_params("missing `taskId`"))?;
    let raw: RawTaskIdParams = serde_json::from_value(params.clone())
        .map_err(|e| McpError::invalid_params(format!("invalid task params: {e}")))?;
    Ok(raw.task_id)
}

fn to_wire_status(s: TaskStatus) -> legacy::TaskStatus {
    match s {
        TaskStatus::Working => legacy::TaskStatus::Working,
        TaskStatus::Completed => legacy::TaskStatus::Completed,
        TaskStatus::Failed => legacy::TaskStatus::Failed,
        TaskStatus::Cancelled => legacy::TaskStatus::Cancelled,
    }
}

fn to_wire_task(s: &TaskSnapshot) -> legacy::Task {
    legacy::Task {
        created_at: s.created_at.clone(),
        last_updated_at: s.last_updated_at.clone(),
        poll_interval: Some(TaskStore::POLL_INTERVAL_MS),
        status: to_wire_status(s.status),
        status_message: s.status_message.clone(),
        task_id: s.task_id.clone(),
        ttl: Some(s.ttl_ms),
    }
}

/// Spec error mapping (tasks.mdx §Error Handling): unknown ids and terminal
/// cancels are `-32602`; capacity exhaustion is an internal `-32603`.
fn task_error_response(id: RequestId, e: &TaskError) -> JsonRpcMessage {
    let (code, message) = match e {
        TaskError::NotFound => (
            -32602,
            "unknown task id (expired, evicted, or never created)",
        ),
        TaskError::AlreadyTerminal => (-32602, "task is already in a terminal status"),
        TaskError::CapacityExhausted => (-32603, "task capacity exhausted; retry later"),
    };
    JsonRpcResponse::error(
        id,
        JsonRpcError {
            code,
            message: message.to_owned(),
            data: None,
        },
    )
    .into()
}

// ---- response builders -------------------------------------------------------

/// Build the `server/discover` response, patching each registered extension's
/// settings into `capabilities.extensions[id]`. The generated `extensions` map
/// holds the draft `JsonObject` newtype; merging arbitrary settings JSON is
/// simplest post-serialization (the same presence-marker patch trick
/// `handle_initialize` uses).
fn discover_response<S: McpServerCore>(
    id: RequestId,
    server: &S,
    router: &MethodRouter<S>,
    supported: &[ProtocolVersion],
    extensions: &[Arc<dyn Extension>],
) -> JsonRpcMessage {
    let result = build_discover_result(server, router, supported);
    let mut value = match serde_json::to_value(&result) {
        Ok(v) => v,
        Err(e) => return error_response(id, &McpError::internal(format!("serialize result: {e}"))),
    };
    if !extensions.is_empty()
        && let Some(caps) = value.get_mut("capabilities").and_then(Value::as_object_mut)
    {
        let ext_map = caps
            .entry("extensions")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(ext_map) = ext_map.as_object_mut() {
            for ext in extensions {
                ext_map.insert(ext.id().to_owned(), ext.settings());
            }
        }
    }
    JsonRpcResponse::success(id, value).into()
}

/// Whether the request's per-request client capabilities declare `ext_id` under
/// `extensions` (SEP-2663 capability negotiation). The draft client stamps its
/// capabilities into `_meta` (lifted into [`RequestContext::client_capabilities`]
/// by [`build_context`]).
fn context_declares_extension(ctx: &RequestContext, ext_id: &str) -> bool {
    ctx.client_capabilities
        .as_ref()
        .and_then(|caps| caps.get("extensions"))
        .and_then(Value::as_object)
        .is_some_and(|exts| exts.contains_key(ext_id))
}

fn build_discover_result<S: McpServerCore>(
    server: &S,
    router: &MethodRouter<S>,
    supported: &[ProtocolVersion],
) -> draft::DiscoverResult {
    // `listChanged`/`subscribe` are true: the subscription registry delivers
    // these for every registered capability (`subscriptions/listen`).
    let capabilities = draft::ServerCapabilities {
        // `completions` is an opaque presence marker (an empty object when the
        // server supports argument autocompletion).
        completions: router
            .has_completions()
            .then(|| draft::JsonObject(BTreeMap::new())),
        experimental: BTreeMap::new(),
        extensions: BTreeMap::new(),
        logging: router
            .has_logging()
            .then(|| draft::JsonObject(BTreeMap::new())),
        prompts: router
            .has_prompts()
            .then_some(draft::ServerCapabilitiesPrompts {
                list_changed: Some(true),
            }),
        resources: router
            .has_resources()
            .then_some(draft::ServerCapabilitiesResources {
                list_changed: Some(true),
                subscribe: Some(true),
            }),
        tools: router
            .has_tools()
            .then_some(draft::ServerCapabilitiesTools {
                list_changed: Some(true),
            }),
    };
    draft::DiscoverResult {
        // Conservative cache defaults, consistent with the list conversions in
        // `neutral.rs`: private + immediately stale. A configurable cache
        // policy is tracked as deferred.
        cache_scope: draft::DiscoverResultCacheScope::Private,
        capabilities,
        instructions: server.instructions(),
        meta: None,
        result_type: neutral::result_type::COMPLETE.to_string(),
        server_info: to_draft_impl(server.server_info()),
        supported_versions: supported.iter().map(|v| v.as_str().to_owned()).collect(),
        ttl_ms: 0,
    }
}

fn to_draft_impl(i: Implementation) -> draft::Implementation {
    draft::Implementation {
        description: None,
        icons: Vec::new(),
        name: i.name,
        title: i.title,
        version: i.version,
        website_url: None,
    }
}

#[derive(Deserialize)]
struct RawClientInfo {
    name: String,
    version: String,
    #[serde(default)]
    title: Option<String>,
}

/// Build the per-request context from the wire frame: version, the draft's
/// per-request client identity/capabilities (`_meta` keys), and propagated
/// user `_meta`. (Transport identity extraction joins with Auth in Phase 7.)
fn build_context(req: &JsonRpcRequest) -> RequestContext {
    let version =
        version::request_protocol_version(req.params.as_ref()).unwrap_or(ProtocolVersion::LATEST);
    let mut ctx = RequestContext::new(version);
    if let Some(meta) = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(Value::as_object)
    {
        ctx.identity = turbomcp_core::meta::extract_identity(meta);
        ctx.trace_context = turbomcp_core::meta::extract_trace_context(meta);
        let (consumed, propagated) = turbomcp_core::meta::partition(meta.clone());
        if let Some(info) = consumed
            .get(meta::keys::CLIENT_INFO)
            .and_then(|v| serde_json::from_value::<RawClientInfo>(v.clone()).ok())
        {
            let mut implementation = Implementation::new(info.name, info.version);
            implementation.title = info.title;
            ctx = ctx.with_client_info(implementation);
        }
        if let Some(caps) = consumed.get(meta::keys::CLIENT_CAPABILITIES) {
            ctx.client_capabilities = Some(caps.clone());
        }
        ctx = ctx.with_propagated_meta(propagated);
    }
    ctx
}

#[derive(Deserialize)]
struct RawCallToolParams {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

fn parse_call_tool_params(params: Option<&Value>) -> Result<neutral::CallToolParams, McpError> {
    let params = params.ok_or_else(|| McpError::invalid_params("tools/call requires params"))?;
    let raw: RawCallToolParams = serde_json::from_value(params.clone())
        .map_err(|e| McpError::invalid_params(format!("invalid tools/call params: {e}")))?;
    Ok(neutral::CallToolParams::new(raw.name, raw.arguments))
}

/// Lenient pagination-cursor extraction: a missing `params` or absent `cursor`
/// is simply a first-page request, never an error.
fn parse_list_params(params: Option<&Value>) -> neutral::ListParams {
    match params.and_then(|p| p.get("cursor")).and_then(Value::as_str) {
        Some(cursor) => neutral::ListParams::with_cursor(cursor),
        None => neutral::ListParams::new(),
    }
}

#[derive(Deserialize)]
struct RawReadResourceParams {
    uri: String,
}

/// Parse `logging/setLevel` params: `{ "level": <RFC 5424 level> }`. An
/// unrecognized level answers `-32602` (logging spec §Error Handling).
fn parse_set_level_params(params: Option<&Value>) -> Result<turbomcp_core::LogLevel, McpError> {
    let level = params
        .and_then(|p| p.get("level"))
        .ok_or_else(|| McpError::invalid_params("logging/setLevel requires a level"))?;
    serde_json::from_value(level.clone())
        .map_err(|_| McpError::invalid_params(format!("invalid log level: {level}")))
}

/// Parse the draft per-request log-level opt-in (`_meta`
/// `io.modelcontextprotocol/logLevel`). `Err` means present but unrecognized
/// — the logging spec says reject the request with `-32602`.
fn extract_log_level(params: Option<&Value>) -> Result<Option<turbomcp_core::LogLevel>, McpError> {
    let Some(value) = params
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(meta::keys::LOG_LEVEL))
    else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|_| McpError::invalid_params(format!("invalid logLevel: {value}")))
}

/// Extract the `uri` field shared by `resources/read|subscribe|unsubscribe`.
fn parse_uri_param(params: Option<&Value>, method: &str) -> Result<String, McpError> {
    let params =
        params.ok_or_else(|| McpError::invalid_params(format!("{method} requires params")))?;
    let raw: RawReadResourceParams = serde_json::from_value(params.clone())
        .map_err(|e| McpError::invalid_params(format!("invalid {method} params: {e}")))?;
    Ok(raw.uri)
}

fn parse_read_resource_params(
    params: Option<&Value>,
) -> Result<neutral::ReadResourceParams, McpError> {
    parse_uri_param(params, methods::request::RESOURCES_READ).map(neutral::ReadResourceParams::new)
}

#[derive(Deserialize)]
struct RawGetPromptParams {
    name: String,
    #[serde(default)]
    arguments: BTreeMap<String, String>,
}

fn parse_get_prompt_params(params: Option<&Value>) -> Result<neutral::GetPromptParams, McpError> {
    let params = params.ok_or_else(|| McpError::invalid_params("prompts/get requires params"))?;
    let raw: RawGetPromptParams = serde_json::from_value(params.clone())
        .map_err(|e| McpError::invalid_params(format!("invalid prompts/get params: {e}")))?;
    Ok(neutral::GetPromptParams::new(raw.name, raw.arguments))
}

#[derive(Deserialize)]
struct RawCompleteParams {
    #[serde(rename = "ref")]
    reference: RawCompletionRef,
    argument: RawCompletionArgument,
    #[serde(default)]
    context: Option<RawCompletionContext>,
}

#[derive(Deserialize)]
struct RawCompletionRef {
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Deserialize)]
struct RawCompletionArgument {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct RawCompletionContext {
    #[serde(default)]
    arguments: BTreeMap<String, String>,
}

fn parse_complete_params(params: Option<&Value>) -> Result<neutral::CompleteParams, McpError> {
    let params =
        params.ok_or_else(|| McpError::invalid_params("completion/complete requires params"))?;
    let raw: RawCompleteParams = serde_json::from_value(params.clone()).map_err(|e| {
        McpError::invalid_params(format!("invalid completion/complete params: {e}"))
    })?;

    // The `ref` is a discriminated union keyed on `type` (`ref/prompt` carries
    // `name`; `ref/resource` carries `uri`).
    let reference = match raw.reference.type_.as_str() {
        "ref/prompt" => neutral::CompletionReference::Prompt {
            name: raw
                .reference
                .name
                .ok_or_else(|| McpError::invalid_params("ref/prompt completion requires `name`"))?,
        },
        "ref/resource" => neutral::CompletionReference::ResourceTemplate {
            uri: raw.reference.uri.ok_or_else(|| {
                McpError::invalid_params("ref/resource completion requires `uri`")
            })?,
        },
        other => {
            return Err(McpError::invalid_params(format!(
                "unknown completion ref type: {other}"
            )));
        }
    };

    let mut out = neutral::CompleteParams::new(
        reference,
        neutral::CompletionArgument::new(raw.argument.name, raw.argument.value),
    );
    if let Some(ctx) = raw.context {
        out.context_arguments = ctx.arguments;
    }
    Ok(out)
}

/// Serialize a wire result into a success response, mapping the (practically
/// impossible) serialization failure to an internal error rather than panicking.
fn ok_value<T: Serialize>(id: RequestId, value: &T) -> JsonRpcMessage {
    match serde_json::to_value(value) {
        Ok(v) => JsonRpcResponse::success(id, v).into(),
        Err(e) => error_response(id, &McpError::internal(format!("serialize result: {e}"))),
    }
}

fn error_response(id: RequestId, err: &McpError) -> JsonRpcMessage {
    JsonRpcResponse::error(id, mcp_to_jsonrpc_error(err)).into()
}

/// `-32003` Missing Required Client Capability (SEP-2663): the client requested
/// an extension's behavior without declaring its capability. The `data` names
/// the required extension so the client can re-declare and retry.
fn missing_capability_response(id: RequestId, extension_id: &str) -> JsonRpcMessage {
    let err = JsonRpcError {
        code: -32003,
        message: "missing required client capability".to_owned(),
        data: Some(serde_json::json!({
            "requiredCapabilities": { "extensions": { extension_id: {} } }
        })),
    };
    JsonRpcResponse::error(id, err).into()
}

fn unsupported_version(
    id: RequestId,
    requested: Option<String>,
    supported: &[ProtocolVersion],
) -> JsonRpcMessage {
    let err = ProtocolError::UnsupportedVersion {
        requested,
        supported: supported.iter().map(|v| v.as_str().to_owned()).collect(),
    };
    err.into_response(id).into()
}
