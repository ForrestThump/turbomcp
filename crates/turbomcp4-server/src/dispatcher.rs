//! [`VersionDispatcher`]: the `tower::Service` that turns a [`JsonRpcMessage`]
//! into a typed handler call and back.
//!
//! It lives here (not in `turbomcp4-protocol`, as an early draft of the plan had
//! it) because it is generic over the user's [`McpServerCore`], which sits above
//! the protocol layer — putting it here keeps the dependency graph acyclic while
//! concentrating *all* per-version branching in one place. Above it (RPC
//! middleware) and below it (typed handlers) are version-agnostic.
//!
//! Per-version status (Phase 2): the modern `DRAFT-2026-v1` path is live; the
//! legacy `2025-11-25` (stateful) path is recognized and stubbed so wiring it in
//! Phase 5 is an additive change, not a redesign.
//!
//! Two pieces are deliberately deferred and noted inline: `_meta`→context
//! extraction will move to a `MetaExtractLayer` (Phase 4), and `poll_ready`
//! backpressure (bounded queue) lands with the writer-actor (Phase 4). Today the
//! dispatcher extracts `_meta` itself and is always ready.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value};
use tower::Service;

use turbomcp4_core::{
    Implementation, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, McpError,
    ProtocolVersion, RequestContext, RequestId,
};
use turbomcp4_protocol::v2026_draft::types as draft;
use turbomcp4_protocol::{methods, neutral, version};
use turbomcp4_service::{ProtocolError, mcp_to_jsonrpc_error};

use crate::context::{
    CallToolContext, CompleteContext, GetPromptContext, ListPromptsContext,
    ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, ReadResourceContext,
};
use crate::router::MethodRouter;
use crate::traits::McpServerCore;

/// The protocol seam for a server: `Service<JsonRpcMessage>`.
///
/// Clone is cheap (the server clones per request; the router is shared behind an
/// `Arc`), so the dispatcher composes under per-connection `tower` stacks.
pub struct VersionDispatcher<S> {
    server: S,
    router: Arc<MethodRouter<S>>,
    supported: Vec<ProtocolVersion>,
}

impl<S: Clone> Clone for VersionDispatcher<S> {
    fn clone(&self) -> Self {
        Self {
            server: self.server.clone(),
            router: Arc::clone(&self.router),
            supported: self.supported.clone(),
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
        }
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
        Box::pin(async move { Ok(handle(server, router, supported, msg).await) })
    }
}

async fn handle<S: McpServerCore>(
    server: S,
    router: Arc<MethodRouter<S>>,
    supported: Vec<ProtocolVersion>,
    msg: JsonRpcMessage,
) -> Option<JsonRpcMessage> {
    match msg {
        JsonRpcMessage::Request(req) => {
            Some(handle_request(server, &router, &supported, req).await)
        }
        JsonRpcMessage::Notification(n) => {
            handle_notification(&n);
            None
        }
        JsonRpcMessage::Response(_) => {
            // A client→server response replies to a server-initiated request
            // (sampling/MRTR). No outbound requests exist until Phase 6.
            tracing::debug!("ignoring unsolicited client->server response");
            None
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
    req: JsonRpcRequest,
) -> JsonRpcMessage {
    let id = req.id.clone();
    let method = req.method.clone();

    match method.as_str() {
        // Version-agnostic methods: a client may call these before it knows
        // which version to pin (discovery) or merely to probe liveness.
        methods::request::DISCOVER => {
            ok_value(id, &build_discover_result(&server, router, supported))
        }
        methods::request::PING => JsonRpcResponse::success(id, serde_json::json!({})).into(),

        // Stateful handshake — recognized, routed in Phase 5.
        methods::request::INITIALIZE => legacy_not_implemented(id, "initialize"),

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
                    dispatch_modern(server, router, &req, method.as_str(), id).await
                }
                VersionRoute::Legacy => legacy_not_implemented(id, method.as_str()),
                VersionRoute::Unsupported(requested) => {
                    unsupported_version(id, requested, supported)
                }
            }
        }

        other => error_response(id, &McpError::method_not_found(other)),
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

async fn dispatch_modern<S: McpServerCore>(
    server: S,
    router: &MethodRouter<S>,
    req: &JsonRpcRequest,
    method: &str,
    id: RequestId,
) -> JsonRpcMessage {
    let ctx = build_context(req);
    let list_params = parse_list_params(req.params.as_ref());
    match method {
        methods::request::TOOLS_LIST => {
            let fut = router.dispatch_list_tools(server, ListToolsContext::new(ctx), list_params);
            finish::<_, draft::ListToolsResult>(id, method, fut).await
        }
        methods::request::TOOLS_CALL => {
            let params = match parse_call_tool_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_call_tool(server, CallToolContext::new(ctx), params);
            finish::<_, draft::CallToolResult>(id, method, fut).await
        }
        methods::request::RESOURCES_LIST => {
            let fut =
                router.dispatch_list_resources(server, ListResourcesContext::new(ctx), list_params);
            finish::<_, draft::ListResourcesResult>(id, method, fut).await
        }
        methods::request::RESOURCES_TEMPLATES_LIST => {
            let fut = router.dispatch_list_resource_templates(
                server,
                ListResourceTemplatesContext::new(ctx),
                list_params,
            );
            finish::<_, draft::ListResourceTemplatesResult>(id, method, fut).await
        }
        methods::request::RESOURCES_READ => {
            let params = match parse_read_resource_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_read_resource(server, ReadResourceContext::new(ctx), params);
            finish::<_, draft::ReadResourceResult>(id, method, fut).await
        }
        methods::request::PROMPTS_LIST => {
            let fut =
                router.dispatch_list_prompts(server, ListPromptsContext::new(ctx), list_params);
            finish::<_, draft::ListPromptsResult>(id, method, fut).await
        }
        methods::request::PROMPTS_GET => {
            let params = match parse_get_prompt_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_get_prompt(server, GetPromptContext::new(ctx), params);
            finish::<_, draft::GetPromptResult>(id, method, fut).await
        }
        methods::request::COMPLETION_COMPLETE => {
            let params = match parse_complete_params(req.params.as_ref()) {
                Ok(p) => p,
                Err(e) => return error_response(id, &e),
            };
            let fut = router.dispatch_complete(server, CompleteContext::new(ctx), params);
            finish::<_, draft::CompleteResult>(id, method, fut).await
        }
        _ => unreachable!("dispatch_modern called with an unrouted method"),
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

fn legacy_not_implemented(id: RequestId, method: &str) -> JsonRpcMessage {
    error_response(
        id,
        &McpError::internal(format!(
            "{method}: legacy 2025-11-25 dispatch not yet implemented (Phase 5)"
        )),
    )
}
