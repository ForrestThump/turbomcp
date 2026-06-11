//! # turbomcp4-transport-http
//!
//! The Streamable HTTP transport: a single MCP endpoint served by **axum 0.8**.
//!
//! Unlike stdio (one long-lived bidirectional byte stream driven by the
//! [`serve`](turbomcp4_service::serve) loop), HTTP is request/response — axum
//! owns the accept loop and concurrency. So this crate is *not* a
//! [`Transport`](turbomcp4_service::Transport) impl; it is a runner that drives
//! an [`McpService`] directly: each `POST` body is decoded to a
//! [`JsonRpcMessage`], handed to a per-request clone of the service, and the
//! reply encoded back.
//!
//! ## Endpoint behavior (dual-stack)
//!
//! - **`POST {path}`** — body is one JSON-RPC message (batches are not supported,
//!   per PLAN §13.1). A request yields `200 application/json` with the response;
//!   a notification yields `202 Accepted` with no body. A modern
//!   `subscriptions/listen` request yields a long-lived
//!   `200 text/event-stream` instead: the acknowledged notification first, then
//!   the opted-in change notifications, with 15s keep-alive comments and
//!   `X-Accel-Buffering: no` so proxies don't buffer (transports spec
//!   §Receiving Messages). Closing the stream ends the subscription.
//! - **`GET {path}`** — `405`. The draft replaced the GET stream with
//!   `subscriptions/listen`; the *legacy* (`2025-11-25`) GET→SSE stream is the
//!   Phase 6d dual-stack deliverable.
//! - **`DELETE {path}`** — `405`. The `2025-11-25` spec lets a server refuse
//!   client-initiated session termination; sessions expire by store eviction.
//!
//! ## Dual-stack request routing (PLAN §11)
//!
//! Modern `DRAFT-2026-v1` requests are stateless (version inside the body's
//! `_meta`) and pass through untouched. The legacy `2025-11-25` stateful path
//! is routed from HTTP headers, asserted toward the dispatcher via internal
//! `_meta` keys (inbound bodies are sanitized first so clients can't forge
//! them):
//!
//! 1. An explicit but unsupported `MCP-Protocol-Version` header → `400`.
//! 2. Body is `initialize` → mint a session id, attach it to the message; on
//!    success the response carries it back as `Mcp-Session-Id`.
//! 3. `Mcp-Session-Id` header present → attach the session id (and, for
//!    version-less bodies, the legacy version) and dispatch; an unknown
//!    session answers `404` so the client re-initializes.
//! 4. `MCP-Protocol-Version: 2025-11-25` without a session id (and not
//!    `initialize`) → `400` (the legacy path requires a session).
//!
//! ## Security
//!
//! - **Origin / DNS-rebinding guard:** a request carrying an `Origin` header that
//!   isn't on the allowlist is rejected with `403`. The default allowlist is
//!   empty, so only `Origin`-less (non-browser) clients pass — the secure default
//!   for a local server. Use [`HttpConfig::allow_origin`] /
//!   [`HttpConfig::allow_any_origin`] to widen it.
//! - **Body limit:** `POST` bodies above [`HttpConfig::max_body_bytes`] (default
//!   1 MiB) are rejected with `413`.
//! - **CORS:** off by default; [`HttpConfig::enable_cors`] adds a permissive
//!   `tower-http` `CorsLayer` (intended for `allow_any_origin` dev setups).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::convert::Infallible;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::CorsLayer;
use turbomcp4_codec::{Codec, DefaultCodec};
use turbomcp4_core::{JsonRpcMessage, ProtocolVersion, meta};
use turbomcp4_service::{CancellationToken, McpService, ProtocolError, outbound};

/// The session header of the `2025-11-25` Streamable HTTP transport.
const HEADER_SESSION_ID: HeaderName = HeaderName::from_static("mcp-session-id");
/// The per-request version header of the `2025-11-25` Streamable HTTP transport.
const HEADER_PROTOCOL_VERSION: HeaderName = HeaderName::from_static("mcp-protocol-version");

/// Buffered events per SSE stream; a consumer this far behind backpressures
/// publishers (the registry awaits `send`).
const SSE_CHANNEL_CAPACITY: usize = 256;
/// Default keep-alive comment interval — short enough to outlive common
/// proxy/LB idle timeouts (often 30–60s).
const DEFAULT_SSE_KEEPALIVE: Duration = Duration::from_secs(15);

/// Where a request's `Origin` header is checked against (DNS-rebinding guard).
#[derive(Clone, Debug)]
enum OriginPolicy {
    /// Reject any request whose `Origin` isn't in this list. An empty list lets
    /// only `Origin`-less (non-browser) clients through — the secure default.
    Allowlist(Vec<String>),
    /// Accept any `Origin` (development only).
    Any,
}

/// Configuration for the HTTP endpoint. Construct with [`HttpConfig::new`] and
/// chain the builder methods.
#[derive(Clone, Debug)]
pub struct HttpConfig {
    path: String,
    max_body_bytes: usize,
    origins: OriginPolicy,
    cors: bool,
    shutdown: CancellationToken,
    sse_keepalive: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            path: "/mcp".to_owned(),
            max_body_bytes: 1 << 20, // 1 MiB
            origins: OriginPolicy::Allowlist(Vec::new()),
            cors: false,
            shutdown: CancellationToken::new(),
            sse_keepalive: DEFAULT_SSE_KEEPALIVE,
        }
    }
}

impl HttpConfig {
    /// Default configuration: `POST /mcp`, 1 MiB body limit, Origin-less only.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the endpoint path (default `/mcp`).
    #[must_use]
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Set the maximum accepted request-body size in bytes (default 1 MiB).
    #[must_use]
    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
    }

    /// Add an allowed `Origin` (exact match, e.g. `https://app.example.com`).
    #[must_use]
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        match &mut self.origins {
            OriginPolicy::Allowlist(list) => list.push(origin.into()),
            OriginPolicy::Any => {}
        }
        self
    }

    /// Accept requests from any `Origin` (development only). Also enables a
    /// permissive CORS layer so browsers can actually use it.
    #[must_use]
    pub fn allow_any_origin(mut self) -> Self {
        self.origins = OriginPolicy::Any;
        self.cors = true;
        self
    }

    /// Toggle the permissive CORS layer (off by default).
    #[must_use]
    pub fn enable_cors(mut self, enabled: bool) -> Self {
        self.cors = enabled;
        self
    }

    /// Provide a cancellation token; firing it triggers axum's graceful shutdown.
    #[must_use]
    pub fn with_shutdown(mut self, shutdown: CancellationToken) -> Self {
        self.shutdown = shutdown;
        self
    }

    /// Set the SSE keep-alive comment interval (default 15s). Keep it shorter
    /// than the idle timeout of any proxy in front of the server.
    #[must_use]
    pub fn sse_keepalive(mut self, interval: Duration) -> Self {
        self.sse_keepalive = interval;
        self
    }
}

/// Errors from running the HTTP transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HttpError {
    /// Binding the listener or running the server failed.
    #[error("http server i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Per-request shared state: the service to dispatch into, the codec, and the
/// Origin policy. Cheap to clone (the service clones per request by contract).
#[derive(Clone)]
struct HttpState<S> {
    service: S,
    codec: DefaultCodec,
    origins: OriginPolicy,
    sse_keepalive: Duration,
}

/// Build the configured axum [`Router`] for `service` without binding a socket —
/// the unit of composition (mount it under a larger app) and the seam tests
/// drive via `tower::ServiceExt::oneshot`.
pub fn router<S>(service: S, config: HttpConfig) -> Router
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    let state = HttpState {
        service,
        codec: DefaultCodec::default(),
        origins: config.origins.clone(),
        sse_keepalive: config.sse_keepalive,
    };
    let app = Router::new()
        .route(
            &config.path,
            post(mcp_post::<S>).get(mcp_get).delete(mcp_delete),
        )
        .layer(DefaultBodyLimit::max(config.max_body_bytes));
    let app = if config.cors {
        app.layer(CorsLayer::permissive())
    } else {
        app
    };
    app.with_state(state)
}

/// Serve `service` over Streamable HTTP on `addr` until the configured shutdown
/// token fires (or forever, with the default token).
///
/// # Errors
/// Returns [`HttpError::Io`] if the listener cannot bind or the server loop fails.
pub async fn serve_http<S>(
    addr: SocketAddr,
    service: S,
    config: HttpConfig,
) -> Result<(), HttpError>
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    let shutdown = config.shutdown.clone();
    let app = router(service, config);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "turbomcp http transport listening");
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

// ---- handlers ----------------------------------------------------------------

async fn mcp_post<S>(State(state): State<HttpState<S>>, headers: HeaderMap, body: Bytes) -> Response
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    if let Some(rejection) = check_origin(&state.origins, &headers) {
        return rejection;
    }

    let mut msg: JsonRpcMessage = match state.codec.decode(&body) {
        Ok(msg) => msg,
        Err(e) => return parse_error_response(&e.to_string()),
    };

    // Internal `_meta` is transport-owned: strip anything the client forged
    // before asserting our own (see `turbomcp4_core::meta::internal`).
    meta::sanitize_inbound(&mut msg);

    // Transport spec: an explicit but invalid/unsupported version header is 400.
    let header_version = headers
        .get(&HEADER_PROTOCOL_VERSION)
        .and_then(|v| v.to_str().ok());
    if let Some(v) = header_version
        && !ProtocolVersion::from_wire(v).is_supported()
    {
        return version_header_rejection(v);
    }
    let session_header = headers
        .get(&HEADER_SESSION_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Dual-stack routing (module docs): mark legacy traffic via internal meta;
    // modern stateless bodies pass through untouched.
    let is_initialize = msg.method() == Some("initialize");
    let mut minted_session = None;
    if is_initialize {
        let sid = uuid::Uuid::new_v4().to_string();
        meta::set_request_meta(&mut msg, meta::internal::SESSION_ID, json!(sid));
        minted_session = Some(sid);
    } else if let Some(sid) = session_header {
        if !message_has_version(&msg) {
            meta::set_request_meta(
                &mut msg,
                meta::keys::PROTOCOL_VERSION,
                json!(ProtocolVersion::V2025_11_25.as_str()),
            );
        }
        meta::set_request_meta(&mut msg, meta::internal::SESSION_ID, json!(sid));
    } else if header_version == Some(ProtocolVersion::V2025_11_25.as_str()) {
        // Declared-legacy request with no session and not initialize: the
        // stateful path requires a session (spec §Session Management).
        return session_required_rejection();
    }

    // A modern `subscriptions/listen` request answers with a long-lived SSE
    // stream rather than a JSON body. (A legacy-stamped or malformed listen
    // comes back from the dispatcher as an error *response*, which the SSE
    // path renders as plain JSON — so the divert is safe on method alone.)
    if msg.method() == Some("subscriptions/listen") {
        return listen_sse(&state, msg).await;
    }

    let mut svc = state.service.clone();
    if let Err(e) = poll_fn(|cx| svc.poll_ready(cx)).await {
        return protocol_error_response(&e);
    }
    match svc.call(msg).await {
        Ok(Some(reply)) => {
            let mut resp = encode_json_response(&state.codec, &reply);
            // A successful initialize hands the minted session back to the
            // client as the Mcp-Session-Id header.
            if let Some(sid) = minted_session
                && matches!(&reply, JsonRpcMessage::Response(r) if r.error.is_none())
                && let Ok(value) = HeaderValue::from_str(&sid)
            {
                resp.headers_mut().insert(HEADER_SESSION_ID, value);
            }
            resp
        }
        Ok(None) => StatusCode::ACCEPTED.into_response(), // notification: no body
        Err(e) => protocol_error_response(&e),
    }
}

/// Open the SSE stream for a `subscriptions/listen` request: register a
/// per-stream writer (under a minted connection id) so the dispatcher's
/// subscription registry can reach this response, dispatch the listen, and —
/// if it was accepted — stream every pushed message as an SSE event.
///
/// The writer registration travels inside the stream state, so a client
/// disconnect (axum drops the body) unregisters it; the registry prunes the
/// subscription at its next publish (transports spec §Cancellation: closing
/// the stream is the cancellation signal).
async fn listen_sse<S>(state: &HttpState<S>, mut msg: JsonRpcMessage) -> Response
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    let connection_id = format!("http-sse-{}", uuid::Uuid::new_v4());
    let (tx, rx) = tokio::sync::mpsc::channel::<JsonRpcMessage>(SSE_CHANNEL_CAPACITY);
    let registration = outbound::register(&connection_id, tx);
    meta::set_request_meta(
        &mut msg,
        meta::internal::CONNECTION_ID,
        json!(connection_id),
    );

    let mut svc = state.service.clone();
    if let Err(e) = poll_fn(|cx| svc.poll_ready(cx)).await {
        return protocol_error_response(&e);
    }
    match svc.call(msg).await {
        // Accepted: no JSON-RPC response; the ack notification is already in
        // the channel as the stream's first event.
        Ok(None) => {}
        // Rejected in-band (bad filter, legacy path, unsupported version).
        Ok(Some(reply)) => return encode_json_response(&state.codec, &reply),
        Err(e) => return protocol_error_response(&e),
    }

    let codec = state.codec;
    let stream = futures::stream::unfold(
        (rx, registration, codec),
        |(mut rx, registration, codec)| async move {
            let msg = rx.recv().await?;
            let event = match codec.encode(&msg) {
                Ok(bytes) => Event::default().data(String::from_utf8_lossy(&bytes)),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to encode SSE event; skipped");
                    Event::default().comment("event encoding failed; skipped")
                }
            };
            Some((Ok::<_, Infallible>(event), (rx, registration, codec)))
        },
    );

    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(state.sse_keepalive)
            .text("keep-alive"),
    );
    // X-Accel-Buffering tells reverse proxies (nginx) not to buffer the
    // stream (transports spec: SHOULD include it on SSE responses).
    (
        [(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        )],
        sse,
    )
        .into_response()
}

/// The draft has no GET stream (`subscriptions/listen` over POST replaced it);
/// the `2025-11-25` GET→SSE server-message stream is the Phase 6d dual-stack
/// deliverable. Until then the spec permits answering `GET` with `405`.
async fn mcp_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "no GET stream: the draft subscribes via subscriptions/listen over POST",
    )
        .into_response()
}

/// The `2025-11-25` spec permits answering session-termination `DELETE` with
/// `405` ("the server does not allow clients to terminate sessions"); sessions
/// are reclaimed by store eviction instead. Explicit termination may land with
/// the Phase 7 hardening pass.
async fn mcp_delete() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "client-initiated session termination is not supported; sessions expire by eviction",
    )
        .into_response()
}

// ---- helpers -----------------------------------------------------------------

/// Whether the message's `params._meta` already states a protocol version (a
/// modern stateless request does; a legacy post-initialize request doesn't).
fn message_has_version(msg: &JsonRpcMessage) -> bool {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_ref(),
        JsonRpcMessage::Notification(n) => n.params.as_ref(),
        JsonRpcMessage::Response(_) => None,
    };
    params
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(meta::keys::PROTOCOL_VERSION))
        .is_some()
}

/// `400` for an explicit but unsupported `MCP-Protocol-Version` header.
fn version_header_rejection(requested: &str) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": -32004,
            "message": format!("unsupported MCP-Protocol-Version header: {requested}"),
        },
    });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// `400` for a declared-legacy request missing its `Mcp-Session-Id`.
fn session_required_rejection() -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {
            "code": -32002,
            "message": "the 2025-11-25 path requires an Mcp-Session-Id header (initialize first)",
        },
    });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// Returns `Some(rejection)` if the request's `Origin` is disallowed, else `None`.
fn check_origin(policy: &OriginPolicy, headers: &HeaderMap) -> Option<Response> {
    let origin = headers.get(header::ORIGIN)?; // no Origin → non-browser → allowed
    match policy {
        OriginPolicy::Any => None,
        OriginPolicy::Allowlist(list) => {
            let origin = origin.to_str().unwrap_or_default();
            (!list.iter().any(|allowed| allowed == origin))
                .then(|| (StatusCode::FORBIDDEN, "origin not allowed").into_response())
        }
    }
}

fn encode_json_response(codec: &DefaultCodec, msg: &JsonRpcMessage) -> Response {
    match codec.encode(msg) {
        Ok(bytes) => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            bytes,
        )
            .into_response(),
        Err(e) => protocol_error_response(&ProtocolError::from(e)),
    }
}

/// A malformed body has no usable id; answer `400` with a JSON-RPC parse error.
fn parse_error_response(detail: &str) -> Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": -32700, "message": format!("parse error: {detail}") },
    });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// Map a service/transport [`ProtocolError`] to an HTTP status + JSON-RPC error
/// body (PLAN §4.10). User `McpError`s never reach here — the dispatcher renders
/// them as `Ok` error *responses*; this is for parse/version/shutdown conditions.
fn protocol_error_response(err: &ProtocolError) -> Response {
    let status = match err {
        ProtocolError::Parse(_)
        | ProtocolError::UnsupportedVersion { .. }
        | ProtocolError::MissingCapability(_) => StatusCode::BAD_REQUEST,
        // Spec §Session Management: an expired/unknown session answers 404 so
        // the client starts over with a fresh initialize.
        ProtocolError::UnknownSession(_) => StatusCode::NOT_FOUND,
        ProtocolError::Transport(_) | ProtocolError::ServerShuttingDown => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": err.jsonrpc_code(), "message": err.to_string() },
    });
    (status, Json(body)).into_response()
}
