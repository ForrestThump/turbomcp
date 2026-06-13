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
//!   per PLAN §13.1). A notification yields `202 Accepted` with no body. A
//!   request yields either `200 application/json` with the response, or — if
//!   the handler emits server→client messages mid-flight (inline bidi
//!   requests on the legacy path, progress, log messages) — a
//!   `200 text/event-stream` *scoped to that request*: the request-related
//!   messages as events, then the final response, which terminates the stream
//!   (transports spec §Sending Messages). A modern `subscriptions/listen`
//!   request yields a long-lived `200 text/event-stream` instead: the
//!   acknowledged notification first, then the opted-in change notifications.
//!   Every SSE response carries keep-alive comments (default 15s) and
//!   `X-Accel-Buffering: no` so proxies don't buffer. Closing a stream is the
//!   cancellation signal for the work it carries.
//! - **`GET {path}`** — with an `Mcp-Session-Id` header: the legacy
//!   (`2025-11-25`) server→client SSE stream for that session (list_changed,
//!   resources/updated). Without one: `405` — the draft replaced the GET
//!   stream with `subscriptions/listen`.
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

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
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
use turbomcp4_core::{JsonRpcMessage, ProtocolVersion, RequestId, meta};
use turbomcp4_service::{
    AuthDecision, CancellationToken, HttpAuthenticator, McpService, ProtocolError, outbound,
};

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

/// The well-known path RFC 9728 Protected Resource Metadata is served at.
const RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

/// Configuration for the HTTP endpoint. Construct with [`HttpConfig::new`] and
/// chain the builder methods.
#[derive(Clone)]
pub struct HttpConfig {
    path: String,
    max_body_bytes: usize,
    origins: OriginPolicy,
    cors: bool,
    shutdown: CancellationToken,
    sse_keepalive: Duration,
    authenticator: Option<Arc<dyn HttpAuthenticator>>,
}

impl core::fmt::Debug for HttpConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpConfig")
            .field("path", &self.path)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("origins", &self.origins)
            .field("cors", &self.cors)
            .field("sse_keepalive", &self.sse_keepalive)
            .field("authenticator", &self.authenticator.is_some())
            .finish()
    }
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
            authenticator: None,
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

    /// Protect the endpoint as an OAuth 2.1 resource server: every `POST`/`GET`
    /// must carry a valid `Authorization: Bearer` token (validated by
    /// `authenticator`, e.g. `turbomcp4_auth::ResourceServer`), and the RFC
    /// 9728 metadata document is served at
    /// `/.well-known/oauth-protected-resource`. Unauthenticated requests get
    /// the `401`/`403` + `WWW-Authenticate` challenges. stdio is unaffected
    /// (the MCP spec has no stdio auth).
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn HttpAuthenticator>) -> Self {
        self.authenticator = Some(authenticator);
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

/// Per-request shared state: the service to dispatch into, the codec, the
/// Origin policy, and the optional resource-server authenticator. Cheap to
/// clone (the service clones per request by contract).
#[derive(Clone)]
struct HttpState<S> {
    service: S,
    codec: DefaultCodec,
    origins: OriginPolicy,
    sse_keepalive: Duration,
    authenticator: Option<Arc<dyn HttpAuthenticator>>,
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
        authenticator: config.authenticator.clone(),
    };
    let mut app = Router::new()
        .route(
            &config.path,
            post(mcp_post::<S>).get(mcp_get::<S>).delete(mcp_delete),
        )
        .layer(DefaultBodyLimit::max(config.max_body_bytes));
    // RFC 9728 Protected Resource Metadata is public (no auth) discovery.
    if config.authenticator.is_some() {
        app = app.route(
            RESOURCE_METADATA_PATH,
            axum::routing::get(resource_metadata::<S>),
        );
    }
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

    // Resource-server auth (if configured): validate the bearer token and
    // inject the principal into internal `_meta` — after sanitize, so a
    // forged identity can't survive. A rejected request never dispatches.
    if let Some(rejection) = enforce_auth(&state, &headers, Some(&mut msg)).await {
        return rejection;
    }

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

    // Every other *request* takes the lazy-upgrade path: plain JSON unless the
    // handler emits server→client messages mid-flight. `initialize` stays on
    // the inline path below — its response must carry the minted session
    // header, and the handshake never streams.
    if !is_initialize && matches!(&msg, JsonRpcMessage::Request(_)) {
        return request_post(&state, msg).await;
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

    sse_response(state.codec, rx, registration, state.sse_keepalive)
}

/// Dispatch one JSON-RPC request with a per-request server→client channel
/// (transports spec §Sending Messages: the server answers each POSTed request
/// with either a single JSON object or an SSE stream scoped to that request).
///
/// The channel is registered in [`outbound`] under a minted per-request
/// connection id before dispatch, so anything the handler emits mid-flight —
/// inline bidi requests on the legacy path, progress, log messages — reaches
/// this response. If nothing is emitted the reply stays plain JSON; the first
/// mid-flight message upgrades the response to `text/event-stream`, carrying
/// the request-related messages followed by the final response, which
/// terminates the stream. A client disconnect drops the in-flight call —
/// HTTP's cancellation signal.
async fn request_post<S>(state: &HttpState<S>, mut msg: JsonRpcMessage) -> Response
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    let JsonRpcMessage::Request(req) = &msg else {
        unreachable!("request_post is only called for requests");
    };
    let request_id = req.id.clone();
    let connection_id = format!("http-post-{}", uuid::Uuid::new_v4());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<JsonRpcMessage>(SSE_CHANNEL_CAPACITY);
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
    let mut call = Box::pin(svc.call(msg));

    tokio::select! {
        biased;
        result = call.as_mut() => {
            // Completed without upgrading — but messages may have raced into
            // the channel just before completion; don't lose them.
            let mut events = drain(&mut rx);
            drop(registration);
            match result {
                Ok(Some(reply)) if events.is_empty() => {
                    encode_json_response(&state.codec, &reply)
                }
                Ok(Some(reply)) => {
                    events.push_back(reply);
                    finished_sse(state.codec, events)
                }
                // A request always gets a response from the dispatcher; these
                // arms are defensive.
                Ok(None) if events.is_empty() => StatusCode::ACCEPTED.into_response(),
                Ok(None) => finished_sse(state.codec, events),
                Err(e) => protocol_error_response(&e),
            }
        }
        first = rx.recv() => {
            let Some(first) = first else {
                // Unreachable while `registration` holds the sender.
                return protocol_error_response(&ProtocolError::Internal(
                    "per-request channel closed while registered".to_owned(),
                ));
            };
            streaming_post_sse(
                state.codec,
                first,
                rx,
                call,
                registration,
                request_id,
                state.sse_keepalive,
            )
        }
    }
}

/// State for an upgraded per-request SSE response: keep streaming channel
/// messages while driving the in-flight call; when the call completes, append
/// its final response and end the stream.
enum PostStream<F> {
    /// The request is still in flight.
    Run {
        rx: tokio::sync::mpsc::Receiver<JsonRpcMessage>,
        call: Pin<Box<F>>,
        id: RequestId,
        registration: outbound::WriterGuard,
    },
    /// The call finished; flush the remaining events and close.
    Tail(VecDeque<JsonRpcMessage>),
}

/// The upgraded per-request SSE response (see [`request_post`]). The final
/// response (or a JSON-RPC error built from a [`ProtocolError`]) is the last
/// event; dropping the response body drops the call future.
fn streaming_post_sse<F>(
    codec: DefaultCodec,
    first: JsonRpcMessage,
    rx: tokio::sync::mpsc::Receiver<JsonRpcMessage>,
    call: Pin<Box<F>>,
    registration: outbound::WriterGuard,
    id: RequestId,
    keepalive: Duration,
) -> Response
where
    F: Future<Output = Result<Option<JsonRpcMessage>, ProtocolError>> + Send + 'static,
{
    let head = futures::stream::iter([Ok::<_, Infallible>(sse_event(&codec, &first))]);
    let tail = futures::stream::unfold(
        PostStream::Run {
            rx,
            call,
            id,
            registration,
        },
        move |state| async move {
            match state {
                PostStream::Run {
                    mut rx,
                    mut call,
                    id,
                    registration,
                } => {
                    tokio::select! {
                        result = call.as_mut() => {
                            let mut events = drain(&mut rx);
                            drop(registration);
                            match result {
                                Ok(Some(reply)) => events.push_back(reply),
                                Ok(None) => {}
                                Err(e) => events.push_back(e.into_response(id).into()),
                            }
                            let msg = events.pop_front()?;
                            Some((
                                Ok::<_, Infallible>(sse_event(&codec, &msg)),
                                PostStream::Tail(events),
                            ))
                        }
                        msg = rx.recv() => {
                            let msg = msg.expect("sender held by registration");
                            Some((
                                Ok::<_, Infallible>(sse_event(&codec, &msg)),
                                PostStream::Run { rx, call, id, registration },
                            ))
                        }
                    }
                }
                PostStream::Tail(mut events) => {
                    let msg = events.pop_front()?;
                    Some((
                        Ok::<_, Infallible>(sse_event(&codec, &msg)),
                        PostStream::Tail(events),
                    ))
                }
            }
        },
    );
    let stream = futures::StreamExt::chain(head, tail);
    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(keepalive).text("keep-alive"));
    (
        [(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        )],
        sse,
    )
        .into_response()
}

/// A short, complete SSE response for a request that finished before the
/// upgrade decision but raced messages into its channel: the messages, the
/// final response, end of stream.
fn finished_sse(codec: DefaultCodec, events: VecDeque<JsonRpcMessage>) -> Response {
    let stream = futures::stream::iter(
        events
            .into_iter()
            .map(move |msg| Ok::<_, Infallible>(sse_event(&codec, &msg))),
    );
    (
        [(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        )],
        Sse::new(stream),
    )
        .into_response()
}

/// Empty the channel without awaiting (post-completion stragglers).
fn drain(rx: &mut tokio::sync::mpsc::Receiver<JsonRpcMessage>) -> VecDeque<JsonRpcMessage> {
    let mut events = VecDeque::new();
    while let Ok(msg) = rx.try_recv() {
        events.push_back(msg);
    }
    events
}

/// Encode one message as one `data:` event; an encode failure becomes a
/// comment so the stream survives.
fn sse_event(codec: &DefaultCodec, msg: &JsonRpcMessage) -> Event {
    match codec.encode(msg) {
        Ok(bytes) => Event::default().data(String::from_utf8_lossy(&bytes)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to encode SSE event; skipped");
            Event::default().comment("event encoding failed; skipped")
        }
    }
}

/// The common SSE response shape for the two long-lived stream kinds (modern
/// listen, legacy GET): every channel message becomes one `data:` event;
/// keep-alive comments flow in between; the writer registration travels inside
/// the stream state so dropping the response body unregisters it.
fn sse_response(
    codec: DefaultCodec,
    rx: tokio::sync::mpsc::Receiver<JsonRpcMessage>,
    registration: outbound::WriterGuard,
    keepalive: Duration,
) -> Response {
    let stream = futures::stream::unfold(
        (rx, registration, codec),
        |(mut rx, registration, codec)| async move {
            let msg = rx.recv().await?;
            let event = sse_event(&codec, &msg);
            Some((Ok::<_, Infallible>(event), (rx, registration, codec)))
        },
    );

    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(keepalive).text("keep-alive"));
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

/// The legacy (`2025-11-25`) server→client SSE stream: `GET` with an
/// `Mcp-Session-Id` opens the session's notification stream (transports spec
/// §Listening for Messages). The stream's writer is registered under the
/// session's [`outbound::session_stream_id`]; a newer GET stream replaces an
/// older one (the spec forbids broadcasting one message across streams).
/// Resumability (`Last-Event-ID`) is not supported.
///
/// The draft never GETs — it subscribes via `subscriptions/listen` over POST —
/// so a session-less GET answers `405`, which the spec permits.
async fn mcp_get<S>(State(state): State<HttpState<S>>, headers: HeaderMap) -> Response
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    if let Some(rejection) = check_origin(&state.origins, &headers) {
        return rejection;
    }
    // The GET stream is part of the protected resource; require auth too.
    if let Some(rejection) = enforce_auth(&state, &headers, None).await {
        return rejection;
    }
    let Some(sid) = headers
        .get(&HEADER_SESSION_ID)
        .and_then(|v| v.to_str().ok())
    else {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST")],
            "no GET stream without Mcp-Session-Id: the draft subscribes via subscriptions/listen",
        )
            .into_response();
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<JsonRpcMessage>(SSE_CHANNEL_CAPACITY);
    let registration = outbound::register(outbound::session_stream_id(sid), tx);
    sse_response(state.codec, rx, registration, state.sse_keepalive)
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

/// Serve the RFC 9728 Protected Resource Metadata document (public, no auth).
/// Only routed when an authenticator is configured.
async fn resource_metadata<S>(State(state): State<HttpState<S>>) -> Response
where
    S: McpService + Clone + Sync,
    S::Future: Send + 'static,
{
    match &state.authenticator {
        Some(authenticator) => Json(authenticator.resource_metadata()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---- auth --------------------------------------------------------------------

/// Enforce resource-server auth when configured. Returns `Some(challenge)` to
/// reject (401/403 + `WWW-Authenticate`); returns `None` to allow — injecting
/// the validated principal into `msg`'s internal `_meta` (when a message is
/// given) so the dispatcher lifts it into the request's identity. A `None`
/// authenticator is an open endpoint (allow).
async fn enforce_auth<S>(
    state: &HttpState<S>,
    headers: &HeaderMap,
    msg: Option<&mut JsonRpcMessage>,
) -> Option<Response> {
    let authenticator = state.authenticator.as_ref()?;
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    match authenticator.authenticate(authorization).await {
        AuthDecision::Allow(principal) => {
            if let Some(msg) = msg {
                meta::set_request_meta(msg, meta::internal::IDENTITY, principal);
            }
            None
        }
        AuthDecision::Challenge {
            status,
            www_authenticate,
        } => Some(challenge_response(status, &www_authenticate)),
    }
}

/// Build an auth-challenge response: the status (401/403) plus the
/// `WWW-Authenticate` header.
fn challenge_response(status: u16, www_authenticate: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::UNAUTHORIZED);
    let header = HeaderValue::from_str(www_authenticate)
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer"));
    (status, [(axum::http::header::WWW_AUTHENTICATE, header)]).into_response()
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
