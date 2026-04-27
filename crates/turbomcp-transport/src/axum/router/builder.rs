//! Router builder implementation
//!
//! This module contains the actual implementation of the AxumMcpExt trait for Router,
//! providing the functionality to add MCP capabilities to Axum applications.

// See `mod.rs` — internal subtree references silenced; deprecation fires for
// external consumers via the source-level `#[deprecated]` attributes.
#![allow(deprecated)]

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{Method, StatusCode},
    middleware,
    routing::{get, post},
};
use tokio::sync::broadcast;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::axum::config::{CorsConfig, McpServerConfig};
use crate::axum::handlers::{
    capabilities_handler, health_handler, json_rpc_handler, metrics_handler, sse_handler,
    websocket_handler,
};
use crate::axum::middleware::{rate_limiting_middleware, security_headers_middleware};
use crate::axum::router::AxumMcpExt;
use crate::axum::service::{McpAppState, McpService};
use crate::tower::{SessionInfo, SessionManager};

#[cfg(any(feature = "auth", feature = "jwt-validation"))]
use crate::axum::middleware::authentication_middleware;

/// Session middleware - adds session tracking to all requests.
async fn session_middleware(
    mut request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    // Create new session for this request
    let mut session = SessionInfo::new();

    // Extract headers and store in session metadata
    for (name, value) in request.headers().iter() {
        if let Ok(value_str) = value.to_str() {
            session
                .metadata
                .insert(name.to_string(), value_str.to_string());
        }
    }

    // Extract specific useful headers
    if let Some(user_agent) = request
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
    {
        session.user_agent = Some(user_agent.to_string());
    }

    if let Some(remote_addr) = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        session.remote_addr = Some(remote_addr.to_string());
    }

    request.extensions_mut().insert(session);
    next.run(request).await
}

/// Origin-allowlist middleware (DNS-rebinding/CSRF defence per MCP spec).
///
/// When `cors.allowed_origins` is a non-wildcard, non-empty list, requests whose
/// `Origin` header is not on the list are rejected with 403. Requests without
/// an `Origin` header (non-browser clients) pass through. Wildcard `"*"` and
/// empty allowlists are permissive — startup logging warns about an empty
/// allowlist when the operator opts into Origin enforcement via CORS config.
///
/// Both the inbound `Origin` header and allowlist entries are canonicalized
/// through `security::origin::canonicalize_origin` (URL parser, scheme/host
/// case-folded, default port filled) so `https://Example.com` matches
/// `https://example.com:443`, and origins with smuggled paths/userinfo
/// (`http://localhost.evil.com`, `http://localhost@evil.com`) are rejected.
async fn origin_validation_middleware(
    State(config): State<Arc<CorsConfig>>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    use crate::security::origin::canonicalize_origin;

    // Determine effective allowlist (None or wildcard means "no enforcement").
    let allowlist: Option<&Vec<String>> = match &config.allowed_origins {
        Some(list) if !list.is_empty() && !list.iter().any(|o| o == "*") => Some(list),
        _ => None,
    };

    if let Some(allowed) = allowlist
        && let Some(origin_value) = request.headers().get(axum::http::header::ORIGIN)
        && let Ok(origin_str) = origin_value.to_str()
    {
        let allowed_match = canonicalize_origin(origin_str).is_some_and(|incoming| {
            allowed
                .iter()
                .filter_map(|entry| canonicalize_origin(entry))
                .any(|entry| entry == incoming)
        });

        if !allowed_match {
            return axum::response::Response::builder()
                .status(StatusCode::FORBIDDEN)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(r#"{"error":"Origin not allowed"}"#))
                .unwrap_or_else(|_| StatusCode::FORBIDDEN.into_response());
        }
    }

    next.run(request).await
}

use axum::response::IntoResponse;

/// Build a CORS layer from configuration.
fn build_cors_layer(cors_config: &CorsConfig) -> CorsLayer {
    let mut cors = CorsLayer::new();

    if !cors_config.allowed_methods.is_empty() {
        let methods: Vec<Method> = cors_config
            .allowed_methods
            .iter()
            .filter_map(|m| m.parse().ok())
            .collect();
        cors = cors.allow_methods(methods);
    }

    // Wildcard origin selected when the allowlist explicitly contains "*" or is
    // unset (`None`). Pair this with `allow_credentials=true` and the W3C CORS
    // spec forbids the response (and `tower-http` panics at layer build time);
    // we refuse the credentials flag below to fail loudly *and* safely.
    let wildcard_origin = match &cors_config.allowed_origins {
        Some(origins) if origins.iter().any(|o| o == "*") => true,
        None => true,
        _ => false,
    };

    match &cors_config.allowed_origins {
        Some(origins) if origins.iter().any(|o| o == "*") => {
            cors = cors.allow_origin(Any);
        }
        Some(origins) if !origins.is_empty() => {
            let origin_list: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
            cors = cors.allow_origin(origin_list);
        }
        Some(_) => {
            // Empty allowlist: configuration error. Warn loudly but don't crash.
            // Operator either forgot to set TURBOMCP_CORS_ORIGINS or means
            // non-browser-only deployment. Default to no allowed origin.
            tracing::warn!(
                "CORS allowed_origins is empty; no browser origins will be accepted. \
                 Set TURBOMCP_CORS_ORIGINS or pass `with_cors_origins(...)`."
            );
        }
        None => {
            cors = cors.allow_origin(Any);
        }
    }

    if !cors_config.allowed_headers.is_empty() {
        let headers: Vec<_> = cors_config
            .allowed_headers
            .iter()
            .filter_map(|h| h.parse().ok())
            .collect();
        cors = cors.allow_headers(headers);
    }

    if cors_config.allow_credentials {
        if wildcard_origin {
            tracing::error!(
                "CORS misconfiguration: allow_credentials=true is incompatible with a \
                 wildcard or unset allowed_origins (W3C CORS forbids the combination). \
                 Ignoring allow_credentials — supply an explicit origin allowlist to \
                 enable credentialed requests."
            );
        } else {
            cors = cors.allow_credentials(true);
        }
    }

    if let Some(max_age) = cors_config.max_age {
        cors = cors.max_age(max_age);
    }

    cors
}

/// Apply middleware stack appropriate for streaming routes (SSE/WS).
///
/// Notably **excludes** `TimeoutLayer` (would terminate long-lived streams) and
/// `CompressionLayer` (tower-http buffers, breaking SSE flushes). Origin and
/// authentication remain in place because the upgrade itself is a request that
/// must be authorized.
fn streaming_middleware<S>(router: Router<S>, config: &McpServerConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let cors = Arc::new(config.cors.clone());

    let mut router = router.layer(middleware::from_fn(session_middleware));

    // Origin validation (DNS-rebinding/CSRF defence) — applies before upgrade.
    if config.cors.enabled {
        router = router.layer(middleware::from_fn_with_state(
            cors.clone(),
            origin_validation_middleware,
        ));
    }

    // Authentication (applied if configured).
    #[cfg(any(feature = "auth", feature = "jwt-validation"))]
    if let Some(auth_config) = &config.auth {
        router = router.layer(middleware::from_fn_with_state(
            auth_config.clone(),
            authentication_middleware,
        ));
    }

    // CORS (preflight, response headers).
    if config.cors.enabled {
        router = router.layer(build_cors_layer(&config.cors));
    }

    if config.enable_tracing {
        router = router.layer(TraceLayer::new_for_http());
    }

    router
}

/// Apply middleware stack appropriate for request/response routes
/// (JSON-RPC POST, capabilities, health, metrics).
fn rpc_middleware<S>(router: Router<S>, config: &McpServerConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let cors = Arc::new(config.cors.clone());

    let mut router = router
        // Body-size limit (CRIT — was previously not enforced).
        .layer(DefaultBodyLimit::max(config.max_request_size))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            config.request_timeout,
        ))
        .layer(middleware::from_fn(session_middleware));

    if config.enable_compression {
        router = router.layer(CompressionLayer::new());
    }

    if config.security.enabled {
        router = router.layer(middleware::from_fn_with_state(
            config.security.clone(),
            security_headers_middleware,
        ));
    }

    if config.rate_limiting.enabled {
        router = router.layer(middleware::from_fn_with_state(
            config.rate_limiting.clone(),
            rate_limiting_middleware,
        ));
    }

    if config.cors.enabled {
        router = router.layer(middleware::from_fn_with_state(
            cors.clone(),
            origin_validation_middleware,
        ));
    }

    #[cfg(any(feature = "auth", feature = "jwt-validation"))]
    if let Some(auth_config) = &config.auth {
        router = router.layer(middleware::from_fn_with_state(
            auth_config.clone(),
            authentication_middleware,
        ));
    }

    if config.cors.enabled {
        router = router.layer(build_cors_layer(&config.cors));
    }

    if config.enable_tracing {
        router = router.layer(TraceLayer::new_for_http());
    }

    router
}

impl<S> AxumMcpExt for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn turbo_mcp_routes_with_config<T: McpService + 'static>(
        self,
        service: T,
        config: McpServerConfig,
    ) -> Router<S> {
        let session_manager = Arc::new(SessionManager::with_config(
            Duration::from_secs(300), // 5 minute session timeout
            config.max_connections,
        ));

        let (sse_sender, _) = broadcast::channel(1000);

        let app_state = McpAppState {
            service: Arc::new(service) as Arc<dyn McpService>,
            session_manager,
            sse_sender,
            config: config.clone(),
        };

        // Streaming routes (SSE/WS) — separate router that excludes timeout/compression.
        let streaming = Router::new()
            .route("/mcp/sse", get(sse_handler))
            .route("/mcp/ws", get(websocket_handler))
            .with_state(app_state.clone());
        let streaming = streaming_middleware(streaming, &config);

        // Request/response routes (JSON-RPC POST + sidecars).
        let rpc = Router::new()
            .route("/mcp", post(json_rpc_handler))
            .route("/mcp/capabilities", get(capabilities_handler))
            .route("/mcp/health", get(health_handler))
            .route("/mcp/metrics", get(metrics_handler))
            .with_state(app_state);
        let rpc = rpc_middleware(rpc, &config);

        // Merge with the caller's existing router.
        self.merge(rpc).merge(streaming)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    fn allowlist_config(origins: Vec<&str>) -> Arc<CorsConfig> {
        Arc::new(CorsConfig {
            enabled: true,
            allowed_origins: Some(origins.into_iter().map(String::from).collect()),
            allowed_methods: vec![],
            allowed_headers: vec![],
            expose_headers: vec![],
            allow_credentials: false,
            max_age: None,
        })
    }

    async fn run_origin_check(config: Arc<CorsConfig>, origin: Option<&str>) -> StatusCode {
        let app =
            Router::new()
                .route("/", get(|| async { "ok" }))
                .layer(middleware::from_fn_with_state(
                    config,
                    origin_validation_middleware,
                ));

        let mut req = Request::builder().uri("/");
        if let Some(o) = origin {
            req = req.header(axum::http::header::ORIGIN, o);
        }
        app.oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn origin_middleware_canonicalizes_case_and_default_port() {
        // Allowlist entry: bare origin without explicit port.
        let config = allowlist_config(vec!["https://example.com"]);

        // Mixed-case host should match.
        assert_eq!(
            run_origin_check(config.clone(), Some("https://Example.com")).await,
            StatusCode::OK
        );
        // Explicit default port should match.
        assert_eq!(
            run_origin_check(config.clone(), Some("https://example.com:443")).await,
            StatusCode::OK
        );
        // A different host must still fail closed.
        assert_eq!(
            run_origin_check(config, Some("https://evil.com")).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn origin_middleware_rejects_smuggled_origins() {
        let config = allowlist_config(vec!["https://example.com"]);

        // Trailing-path / userinfo / lookalike-subdomain origins should not match
        // even though `starts_with` or naive comparisons might let them through.
        for sneaky in [
            "https://example.com/extra",
            "https://example.com@evil.com",
            "https://example.com.evil.com",
            "https://example.com?x=1",
        ] {
            assert_eq!(
                run_origin_check(config.clone(), Some(sneaky)).await,
                StatusCode::FORBIDDEN,
                "origin '{sneaky}' should be rejected",
            );
        }
    }

    #[test]
    fn build_cors_layer_drops_credentials_with_wildcard() {
        // Operator footgun: wildcard origin + allow_credentials. tower-http would
        // panic at layer-build time; we instead suppress credentials and log.
        // This is purely a no-panic / no-construction-error sanity check — the
        // log itself is verified by tracing-test elsewhere.
        let cors_config = CorsConfig {
            enabled: true,
            allowed_origins: Some(vec!["*".to_string()]),
            allowed_methods: vec![],
            allowed_headers: vec![],
            expose_headers: vec![],
            allow_credentials: true,
            max_age: None,
        };
        let _layer = build_cors_layer(&cors_config);

        // Same shape with `None` allowed_origins — also resolves to wildcard.
        let cors_config_none = CorsConfig {
            allowed_origins: None,
            ..cors_config
        };
        let _layer_none = build_cors_layer(&cors_config_none);
    }
}
