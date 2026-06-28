//! Resource-server validation end-to-end: sign a token, validate it through a
//! `ResourceServer`, and assert the spec's accept/reject behavior — audience
//! binding, issuer, expiry, signature, scope, and the `WWW-Authenticate`
//! challenges.
//!
//! Tokens are HS256 over a symmetric (`oct`) JWK: the validation path
//! (header `kid` → JWK lookup → `decode` with `aud`/`iss`/`exp`/leeway) is
//! identical to RS256, without needing RSA keygen in-test.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use turbomcp_auth::{JwtValidator, ResourceMetadata, ResourceServer, StaticJwks};
use turbomcp_service::{AuthDecision, HttpAuthenticator};

const SECRET: &[u8] = b"test-hmac-secret-key-thirty-two!";
const KID: &str = "test-1";
const RESOURCE: &str = "https://mcp.example.com";
const ISSUER: &str = "https://auth.example.com";
const METADATA_URL: &str = "https://mcp.example.com/.well-known/oauth-protected-resource";

fn jwks_json() -> String {
    let k = URL_SAFE_NO_PAD.encode(SECRET);
    json!({ "keys": [ { "kty": "oct", "k": k, "alg": "HS256", "kid": KID } ]}).to_string()
}

/// A `ResourceServer` over the test JWKS, requiring `mcp:use`.
fn resource_server() -> ResourceServer<JwtValidator<StaticJwks>> {
    let jwks = StaticJwks::from_json(&jwks_json()).expect("valid jwks");
    let validator = JwtValidator::new(jwks, RESOURCE, ISSUER).algorithms(vec![Algorithm::HS256]);
    let metadata = ResourceMetadata::new(RESOURCE, [ISSUER]).scopes_supported(["mcp:use"]);
    ResourceServer::new(validator, metadata, METADATA_URL).required_scopes(["mcp:use"])
}

/// Far-future expiry so valid tokens stay valid.
fn future_exp() -> i64 {
    4_102_444_800 // 2100-01-01
}

fn sign(claims: Value) -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    encode(&header, &claims, &EncodingKey::from_secret(SECRET)).expect("sign")
}

fn good_claims() -> Value {
    json!({
        "sub": "user-42",
        "aud": RESOURCE,
        "iss": ISSUER,
        "exp": future_exp(),
        "scope": "mcp:use files:read",
    })
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[tokio::test]
async fn valid_token_is_allowed_with_identity() {
    let rs = resource_server();
    let token = sign(good_claims());
    let decision = rs.authenticate(Some(&bearer(&token))).await;
    match decision {
        AuthDecision::Allow(principal) => {
            assert_eq!(principal["sub"], "user-42");
            assert_eq!(principal["claims"]["iss"], ISSUER);
            assert_eq!(principal["claims"]["aud"], RESOURCE);
        }
        AuthDecision::Challenge { status, .. } => panic!("expected Allow, got {status}"),
    }
}

#[tokio::test]
async fn missing_header_is_bare_401() {
    let rs = resource_server();
    let AuthDecision::Challenge {
        status,
        www_authenticate,
    } = rs.authenticate(None).await
    else {
        panic!("expected challenge");
    };
    assert_eq!(status, 401);
    // No token presented → no `error=`, but the metadata pointer is there.
    assert!(!www_authenticate.contains("error="));
    assert!(www_authenticate.contains(&format!("resource_metadata=\"{METADATA_URL}\"")));
    assert!(www_authenticate.contains("scope=\"mcp:use\""));
}

#[tokio::test]
async fn wrong_audience_is_401_invalid_token() {
    let rs = resource_server();
    let mut claims = good_claims();
    claims["aud"] = json!("https://other.example.com");
    let AuthDecision::Challenge {
        status,
        www_authenticate,
    } = rs.authenticate(Some(&bearer(&sign(claims)))).await
    else {
        panic!("expected challenge");
    };
    assert_eq!(status, 401);
    assert!(www_authenticate.contains("error=\"invalid_token\""));
}

#[tokio::test]
async fn wrong_issuer_is_rejected() {
    let rs = resource_server();
    let mut claims = good_claims();
    claims["iss"] = json!("https://evil.example.com");
    assert!(matches!(
        rs.authenticate(Some(&bearer(&sign(claims)))).await,
        AuthDecision::Challenge { status: 401, .. }
    ));
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let rs = resource_server();
    let mut claims = good_claims();
    claims["exp"] = json!(1_000_000_000); // 2001
    assert!(matches!(
        rs.authenticate(Some(&bearer(&sign(claims)))).await,
        AuthDecision::Challenge { status: 401, .. }
    ));
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let rs = resource_server();
    let token = sign(good_claims());
    // Tamper with the payload segment.
    let mut parts: Vec<&str> = token.split('.').collect();
    parts[2] = "AAAAtampered_signatureAAAA";
    let tampered = parts.join(".");
    assert!(matches!(
        rs.authenticate(Some(&bearer(&tampered))).await,
        AuthDecision::Challenge { status: 401, .. }
    ));
}

#[tokio::test]
async fn insufficient_scope_is_403() {
    let rs = resource_server();
    let mut claims = good_claims();
    claims["scope"] = json!("files:read"); // lacks mcp:use
    let AuthDecision::Challenge {
        status,
        www_authenticate,
    } = rs.authenticate(Some(&bearer(&sign(claims)))).await
    else {
        panic!("expected challenge");
    };
    assert_eq!(status, 403);
    assert!(www_authenticate.contains("error=\"insufficient_scope\""));
    assert!(www_authenticate.contains("scope=\"mcp:use\""));
}

#[tokio::test]
async fn malformed_header_is_401_invalid_token() {
    let rs = resource_server();
    let AuthDecision::Challenge {
        status,
        www_authenticate,
    } = rs.authenticate(Some("Basic abc123")).await
    else {
        panic!("expected challenge");
    };
    assert_eq!(status, 401);
    assert!(www_authenticate.contains("error=\"invalid_token\""));
}

#[test]
fn resource_metadata_document_shape() {
    let rs = resource_server();
    let doc = rs.resource_metadata();
    assert_eq!(doc["resource"], RESOURCE);
    assert_eq!(doc["authorization_servers"][0], ISSUER);
    assert_eq!(doc["scopes_supported"][0], "mcp:use");
    assert_eq!(doc["bearer_methods_supported"][0], "header");
}
