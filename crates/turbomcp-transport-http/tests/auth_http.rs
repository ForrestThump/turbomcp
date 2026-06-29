//! Resource-server auth wired into the HTTP transport: the RFC 9728 metadata
//! endpoint, 401 on unauthenticated requests, and a valid bearer token both
//! passing AND reaching the handler as `ctx.base.identity`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;
use turbomcp_auth::{JwtValidator, ResourceMetadata, ResourceServer, StaticJwks};
use turbomcp_core::{Implementation, McpResult};
use turbomcp_protocol::neutral;
use turbomcp_server::{
    CallToolContext, ListToolsContext, McpServerCore, MethodRouter, VersionDispatcher, WithTools,
};
use turbomcp_transport_http::{HttpConfig, router};

const SECRET: &[u8] = b"test-hmac-secret-key-thirty-two!";
const KID: &str = "test-1";
const RESOURCE: &str = "https://mcp.example.com";
const ISSUER: &str = "https://auth.example.com";
const METADATA_URL: &str = "https://mcp.example.com/.well-known/oauth-protected-resource";

/// A tool that echoes the authenticated subject — proves identity reached the
/// handler through the dispatcher.
#[derive(Clone)]
struct Whoami;

impl McpServerCore for Whoami {
    fn server_info(&self) -> Implementation {
        Implementation::new("whoami", "0.1.0")
    }
}

impl WithTools for Whoami {
    async fn list_tools(
        &self,
        _ctx: &ListToolsContext,
        _params: neutral::ListParams,
    ) -> McpResult<neutral::ListToolsResult> {
        Ok(neutral::ListToolsResult::new(vec![]))
    }

    async fn call_tool(
        &self,
        ctx: &CallToolContext,
        _params: neutral::CallToolParams,
    ) -> McpResult<neutral::CallToolResult> {
        let who = ctx
            .base
            .identity
            .subject()
            .unwrap_or("anonymous")
            .to_owned();
        Ok(neutral::CallToolResult::text(who))
    }
}

fn app() -> axum::Router {
    let k = URL_SAFE_NO_PAD.encode(SECRET);
    let jwks = StaticJwks::from_json(
        &json!({ "keys": [ { "kty": "oct", "k": k, "alg": "HS256", "kid": KID } ]}).to_string(),
    )
    .unwrap();
    let validator = JwtValidator::new(jwks, RESOURCE, ISSUER).algorithms(vec![Algorithm::HS256]);
    let metadata = ResourceMetadata::new(RESOURCE, [ISSUER]);
    let rs = ResourceServer::new(validator, metadata, METADATA_URL);
    let dispatcher = VersionDispatcher::new(Whoami, MethodRouter::new().with_tools());
    router(
        dispatcher,
        HttpConfig::new().with_authenticator(Arc::new(rs)),
    )
}

fn token() -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    let claims = json!({
        "sub": "alice", "aud": RESOURCE, "iss": ISSUER, "exp": 4_102_444_800i64,
    });
    encode(&header, &claims, &EncodingKey::from_secret(SECRET)).unwrap()
}

fn call_request(auth: Option<&str>) -> Request<Body> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "whoami", "arguments": {},
            "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" },
        }
    });
    let mut req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    req.body(Body::from(body.to_string())).unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn metadata_endpoint_is_public() {
    let req = Request::builder()
        .method("GET")
        .uri("/.well-known/oauth-protected-resource")
        .body(Body::empty())
        .unwrap();
    let resp = app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let doc = body_json(resp).await;
    assert_eq!(doc["resource"], RESOURCE);
    assert_eq!(doc["authorization_servers"][0], ISSUER);
}

#[tokio::test]
async fn unauthenticated_post_is_401_with_challenge() {
    let resp = app().oneshot(call_request(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp.headers()[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(challenge.starts_with("Bearer"));
    assert!(challenge.contains(&format!("resource_metadata=\"{METADATA_URL}\"")));
    // No token presented → no error= code.
    assert!(!challenge.contains("error="));
}

#[tokio::test]
async fn invalid_token_is_401_invalid_token() {
    let resp = app()
        .oneshot(call_request(Some("Bearer not-a-jwt")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let challenge = resp.headers()[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(challenge.contains("error=\"invalid_token\""));
}

#[tokio::test]
async fn valid_token_authorizes_and_reaches_handler_identity() {
    let auth = format!("Bearer {}", token());
    let resp = app().oneshot(call_request(Some(&auth))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = body_json(resp).await;
    // The handler echoed ctx.base.identity.subject().
    assert_eq!(result["result"]["content"][0]["text"], "alice");
}

#[tokio::test]
async fn forged_identity_meta_is_stripped_before_auth() {
    // A client tries to assert identity directly via internal _meta. The
    // boundary sanitizes it, and (no token) the request is 401 — the forged
    // subject never reaches the handler.
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "whoami", "arguments": {},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.turbomcp.internal/identity": { "sub": "admin", "claims": {} },
            }
        }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
