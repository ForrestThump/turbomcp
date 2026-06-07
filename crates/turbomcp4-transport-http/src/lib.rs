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
//! ## Endpoint behavior (modern, stateless `DRAFT-2026-v1` path)
//!
//! - **`POST {path}`** — body is one JSON-RPC message (batches are not supported,
//!   per PLAN §13.1). A request yields `200 application/json` with the response;
//!   a notification yields `202 Accepted` with no body.
//! - **`GET {path}`** — `405`. The server→client SSE stream (for
//!   `subscriptions/listen`) lands in Phase 6; until then this endpoint offers no
//!   stream, which the spec permits answering with 405.
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

use std::future::poll_fn;
use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use tower_http::cors::CorsLayer;
use turbomcp4_codec::{Codec, DefaultCodec};
use turbomcp4_core::JsonRpcMessage;
use turbomcp4_service::{CancellationToken, McpService, ProtocolError};

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
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            path: "/mcp".to_owned(),
            max_body_bytes: 1 << 20, // 1 MiB
            origins: OriginPolicy::Allowlist(Vec::new()),
            cors: false,
            shutdown: CancellationToken::new(),
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
    };
    let app = Router::new()
        .route(&config.path, post(mcp_post::<S>).get(mcp_get))
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

    let msg: JsonRpcMessage = match state.codec.decode(&body) {
        Ok(msg) => msg,
        Err(e) => return parse_error_response(&e.to_string()),
    };

    let mut svc = state.service.clone();
    if let Err(e) = poll_fn(|cx| svc.poll_ready(cx)).await {
        return protocol_error_response(&e);
    }
    match svc.call(msg).await {
        Ok(Some(reply)) => encode_json_response(&state.codec, &reply),
        Ok(None) => StatusCode::ACCEPTED.into_response(), // notification: no body
        Err(e) => protocol_error_response(&e),
    }
}

/// The server→client SSE stream is a Phase 6 (subscriptions) deliverable. Until
/// then this endpoint offers no stream; the spec permits answering `GET` with
/// `405`. Phase 6 replaces this with a long-lived `text/event-stream` response
/// carrying `X-Accel-Buffering: no` and `Connection: keep-alive`.
async fn mcp_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "server→client SSE stream not available until subscriptions (Phase 6)",
    )
        .into_response()
}

// ---- helpers -----------------------------------------------------------------

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
