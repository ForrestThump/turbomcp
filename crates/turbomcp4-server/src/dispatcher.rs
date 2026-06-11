//! [`VersionDispatcher`]: the `tower::Service` that turns a [`JsonRpcMessage`]
//! into a typed handler call and back.
//!
//! It lives here (not in `turbomcp4-protocol`, as an early draft of the plan had
//! it) because it is generic over the user's [`McpServerCore`], which sits above
//! the protocol layer — putting it here keeps the dependency graph acyclic while
//! concentrating *all* per-version branching in one place. Above it (RPC
//! middleware) and below it (typed handlers) are version-agnostic.
//!
//! Per-version status (Phase 5): both paths are live. The modern
//! `DRAFT-2026-v1` path is stateless (version in each request's `_meta`); the
//! legacy `2025-11-25` path is stateful — `initialize` negotiates a version and
//! mints a session (via the transport-supplied internal session id, see
//! [`turbomcp4_core::meta::internal`]), and later requests are dispatched with
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

use turbomcp4_core::{
    CancellationToken, Implementation, JsonRpcError, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, McpError, ProtocolVersion, RequestContext, RequestId, meta,
};
use turbomcp4_protocol::v2025_11_25::types as legacy;
use turbomcp4_protocol::v2026_draft::types as draft;
use turbomcp4_protocol::{methods, neutral, version};
use turbomcp4_service::{ProtocolError, mcp_to_jsonrpc_error};

use crate::context::{
    CallToolContext, CompleteContext, GetPromptContext, ListPromptsContext,
    ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, ReadResourceContext,
};
use crate::router::MethodRouter;
use crate::session::{SessionState, SessionStore};
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
    sessions: Arc<SessionStore>,
    tasks: Option<Arc<TaskStore>>,
}

impl<S: Clone> Clone for VersionDispatcher<S> {
    fn clone(&self) -> Self {
        Self {
            server: self.server.clone(),
            router: Arc::clone(&self.router),
            supported: self.supported.clone(),
            sessions: Arc::clone(&self.sessions),
            tasks: self.tasks.clone(),
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
            sessions: Arc::new(SessionStore::default()),
            tasks: None,
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
        self.tasks = Some(Arc::new(TaskStore::default()));
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
        let sessions = Arc::clone(&self.sessions);
        let tasks = self.tasks.clone();
        Box::pin(async move { handle(server, router, supported, sessions, tasks, msg).await })
    }
}

async fn handle<S: McpServerCore>(
    server: S,
    router: Arc<MethodRouter<S>>,
    supported: Vec<ProtocolVersion>,
    sessions: Arc<SessionStore>,
    tasks: Option<Arc<TaskStore>>,
    msg: JsonRpcMessage,
) -> Result<Option<JsonRpcMessage>, ProtocolError> {
    match msg {
        JsonRpcMessage::Request(req) => {
            handle_request(server, &router, &supported, &sessions, tasks.as_ref(), req)
                .await
                .map(Some)
        }
        JsonRpcMessage::Notification(n) => {
            handle_notification(&n);
            Ok(None)
        }
        JsonRpcMessage::Response(_) => {
            // A client→server response replies to a server-initiated request
            // (sampling/MRTR). No outbound requests exist until Phase 6.
            tracing::debug!("ignoring unsolicited client->server response");
            Ok(None)
        }
    }
}

fn handle_notification(n: &JsonRpcNotification) {
    match n.method.as_str() {
        methods::notification::CANCELLED => {
            // Cancellation wiring (look up in-flight request, fire its token)
            // needs the in-flight registry built in Phase 6. Logged for now.
            tracing::debug!("received notifications/cancelled (no-op until Phase 6)");
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
    sessions: &SessionStore,
    tasks: Option<&Arc<TaskStore>>,
    req: JsonRpcRequest,
) -> Result<JsonRpcMessage, ProtocolError> {
    let id = req.id.clone();
    let method = req.method.clone();

    match method.as_str() {
        // Version-agnostic methods: a client may call these before it knows
        // which version to pin (discovery) or merely to probe liveness.
        methods::request::DISCOVER => Ok(ok_value(
            id,
            &build_discover_result(&server, router, supported),
        )),
        methods::request::PING => Ok(JsonRpcResponse::success(id, serde_json::json!({})).into()),

        // Stateful handshake (2025-11-25 and earlier).
        methods::request::INITIALIZE => {
            let tasks_enabled = tasks.is_some() && router.has_tools();
            Ok(handle_initialize(
                &server,
                router,
                supported,
                sessions,
                tasks_enabled,
                &req,
            ))
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
                    let ctx = build_context(&req);
                    Ok(dispatch_capability::<S, DraftWire>(server, router, &req, &ctx, id).await)
                }
                VersionRoute::Legacy => {
                    let ctx = match legacy_context(sessions, &req)? {
                        Ok(ctx) => ctx,
                        Err(response) => return Ok(response),
                    };
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
                    Ok(dispatch_capability::<S, LegacyWire>(server, router, &req, &ctx, id).await)
                }
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
/// `turbomcp4_protocol::neutral`).
trait WireFamily {
    type ListTools: Serialize + From<neutral::ListToolsResult>;
    type CallTool: Serialize + From<neutral::CallToolResult>;
    type ListResources: Serialize + From<neutral::ListResourcesResult>;
    type ListResourceTemplates: Serialize + From<neutral::ListResourceTemplatesResult>;
    type ReadResource: Serialize + From<neutral::ReadResourceResult>;
    type ListPrompts: Serialize + From<neutral::ListPromptsResult>;
    type GetPrompt: Serialize + From<neutral::GetPromptResult>;
    type Complete: Serialize + From<neutral::CompleteResult>;
}

/// `DRAFT-2026-v1` (modern, stateless).
struct DraftWire;

impl WireFamily for DraftWire {
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
    id: RequestId,
) -> JsonRpcMessage {
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
            let fut = router.dispatch_call_tool(server, CallToolContext::new(ctx), params);
            finish::<_, W::CallTool>(id, method, fut).await
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
            let fut = router.dispatch_read_resource(server, ReadResourceContext::new(ctx), params);
            finish::<_, W::ReadResource>(id, method, fut).await
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
            let fut = router.dispatch_get_prompt(server, GetPromptContext::new(ctx), params);
            finish::<_, W::GetPrompt>(id, method, fut).await
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
    if let Some(m) = req
        .params
        .as_ref()
        .and_then(|p| p.get("_meta"))
        .and_then(Value::as_object)
    {
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
                list_changed: Some(false),
            }),
        resources: router
            .has_resources()
            .then_some(legacy::ServerCapabilitiesResources {
                list_changed: Some(false),
                subscribe: Some(false),
            }),
        tasks: None,
        tools: router
            .has_tools()
            .then_some(legacy::ServerCapabilitiesTools {
                list_changed: Some(false),
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

fn build_discover_result<S: McpServerCore>(
    server: &S,
    router: &MethodRouter<S>,
    supported: &[ProtocolVersion],
) -> draft::DiscoverResult {
    let capabilities = draft::ServerCapabilities {
        // `completions` is an opaque presence marker (an empty object when the
        // server supports argument autocompletion).
        completions: router
            .has_completions()
            .then(|| draft::JsonObject(BTreeMap::new())),
        experimental: BTreeMap::new(),
        extensions: BTreeMap::new(),
        logging: None,
        prompts: router
            .has_prompts()
            .then_some(draft::ServerCapabilitiesPrompts {
                list_changed: Some(false),
            }),
        resources: router
            .has_resources()
            .then_some(draft::ServerCapabilitiesResources {
                list_changed: Some(false),
                subscribe: Some(false),
            }),
        tools: router
            .has_tools()
            .then_some(draft::ServerCapabilitiesTools {
                list_changed: Some(false),
            }),
    };
    draft::DiscoverResult {
        capabilities,
        instructions: server.instructions(),
        meta: None,
        result_type: draft::ResultType::Complete,
        server_info: to_draft_impl(server.server_info()),
        supported_versions: supported.iter().map(|v| v.as_str().to_owned()).collect(),
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

/// Build the per-request context from the wire frame (Phase 2: version +
/// propagated `_meta`; identity/client-info extraction joins in Phase 4/7).
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
        let (_consumed, propagated) = turbomcp4_core::meta::partition(meta.clone());
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

fn parse_read_resource_params(
    params: Option<&Value>,
) -> Result<neutral::ReadResourceParams, McpError> {
    let params =
        params.ok_or_else(|| McpError::invalid_params("resources/read requires params"))?;
    let raw: RawReadResourceParams = serde_json::from_value(params.clone())
        .map_err(|e| McpError::invalid_params(format!("invalid resources/read params: {e}")))?;
    Ok(neutral::ReadResourceParams::new(raw.uri))
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
