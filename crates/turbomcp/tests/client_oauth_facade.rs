//! The `client-oauth` facade surface: the feature wiring compiles and the
//! re-exported `turbomcp::auth::client` flow types work through the facade
//! path (the full network flow is covered in `turbomcp-auth`'s own tests).

#![cfg(feature = "client-oauth")]

use turbomcp::auth::client::{
    CallbackParams, ClientCredentials, MemoryCredentialStore, OAuthClient, parse_bearer_challenge,
};

#[test]
fn bearer_challenge_parsing_via_the_facade() {
    let challenge = parse_bearer_challenge(
        r#"Bearer error="insufficient_scope", scope="mcp:use files:read", resource_metadata="https://rs.example/.well-known/oauth-protected-resource""#,
    )
    .expect("parses");
    assert!(challenge.is_insufficient_scope());
    assert_eq!(challenge.scopes(), vec!["mcp:use", "files:read"]);
}

#[test]
fn scope_step_up_merges_previous_and_challenged() {
    let merged = OAuthClient::step_up_scopes(
        &["mcp:use".to_owned()],
        &["files:read".to_owned(), "mcp:use".to_owned()],
    );
    assert!(merged.contains(&"mcp:use".to_owned()));
    assert!(merged.contains(&"files:read".to_owned()));
}

#[test]
fn callback_params_parse_from_a_redirect_url() {
    let params = CallbackParams::from_query(
        "https://app.example/cb?code=abc123&state=xyz&iss=https%3A%2F%2Fas.example",
    );
    assert_eq!(params.code.as_deref(), Some("abc123"));
    assert_eq!(params.state.as_deref(), Some("xyz"));
}

#[test]
fn public_client_credentials_and_store_construct() {
    let creds = ClientCredentials::public("my-native-app");
    let _store = MemoryCredentialStore::default();
    let _ = creds;
}
