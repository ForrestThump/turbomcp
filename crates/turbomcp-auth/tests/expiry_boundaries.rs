//! Expiry boundary tests for the authentication layer.
//!
//! JWT `iat`/`exp`/`nbf` claims are whole Unix seconds. The chrono -> jiff
//! migration must not move the boundary at which a token counts as expired,
//! so the exact second at which the flip happens is pinned here.

use std::time::Duration;

use turbomcp_auth::context::AuthError;
use turbomcp_auth::{AuthContext, UserInfo, ValidationConfig};

/// The current Unix time in whole seconds.
fn now_seconds() -> u64 {
    jiff::Timestamp::now().as_second().max(0) as u64
}

fn user() -> UserInfo {
    UserInfo {
        id: "user-1".to_string(),
        username: "user-1".to_string(),
        email: None,
        display_name: None,
        avatar_url: None,
        metadata: Default::default(),
    }
}

fn context_with_exp(exp: u64) -> AuthContext {
    AuthContext::builder()
        .subject("user-1")
        .user(user())
        .provider("test")
        .exp(exp)
        .build()
        .expect("context builds")
}

#[test]
fn integer_second_claims_match_the_wall_clock() {
    let now = now_seconds();
    let system = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_secs();
    assert!((now as i64 - system as i64).abs() <= 1);
}

#[test]
fn token_is_not_expired_before_the_expiry_second() {
    let exp = now_seconds() + 3600;
    assert!(!context_with_exp(exp).is_expired());
}

#[test]
fn token_is_expired_at_the_expiry_second() {
    // `exp` is a whole second and the comparison is
    // `SystemTime::now() > UNIX_EPOCH + exp`. `SystemTime::now()` carries
    // sub-second precision, so a token whose `exp` is the current second is
    // already expired.
    let exp = now_seconds();
    assert!(context_with_exp(exp).is_expired());

    // And one second earlier it is expired as well.
    let exp = now_seconds() - 1;
    assert!(context_with_exp(exp).is_expired());
}

#[test]
fn validation_rejects_an_expired_token() {
    let config = ValidationConfig {
        leeway: Duration::ZERO,
        validate_exp: true,
        validate_nbf: false,
        ..ValidationConfig::default()
    };

    let expired = context_with_exp(now_seconds() - 1);
    assert!(matches!(
        expired.validate(&config),
        Err(AuthError::TokenExpired)
    ));

    let valid = context_with_exp(now_seconds() + 60);
    assert!(valid.validate(&config).is_ok());
}

#[test]
fn validation_honours_the_expiry_leeway() {
    let config = ValidationConfig {
        leeway: Duration::from_secs(60),
        validate_exp: true,
        validate_nbf: false,
        ..ValidationConfig::default()
    };

    // Expired one second ago, but inside the 60 second leeway.
    let within_leeway = context_with_exp(now_seconds() - 1);
    assert!(within_leeway.validate(&config).is_ok());

    // Expired well before the leeway window.
    let outside_leeway = context_with_exp(now_seconds() - 120);
    assert!(matches!(
        outside_leeway.validate(&config),
        Err(AuthError::TokenExpired)
    ));
}

#[test]
fn validation_rejects_a_token_that_is_not_yet_valid() {
    let config = ValidationConfig {
        leeway: Duration::ZERO,
        validate_exp: false,
        validate_nbf: true,
        ..ValidationConfig::default()
    };

    let context = AuthContext::builder()
        .subject("user-1")
        .user(user())
        .provider("test")
        .nbf(now_seconds() + 60)
        .build()
        .expect("context builds");
    assert!(matches!(
        context.validate(&config),
        Err(AuthError::TokenNotYetValid)
    ));

    // A token that became valid exactly now is accepted.
    let context = AuthContext::builder()
        .subject("user-1")
        .user(user())
        .provider("test")
        .nbf(now_seconds())
        .build()
        .expect("context builds");
    assert!(context.validate(&config).is_ok());
}

#[test]
fn claims_round_trip_as_integer_seconds() {
    let exp = now_seconds() + 3600;
    let iat = now_seconds();
    let context = AuthContext::builder()
        .subject("user-1")
        .user(user())
        .provider("test")
        .exp(exp)
        .iat(iat)
        .build()
        .expect("context builds");

    let claims = context.to_jwt_claims();
    assert_eq!(claims["exp"], serde_json::json!(exp));
    assert_eq!(claims["iat"], serde_json::json!(iat));
    assert!(claims["exp"].is_u64());
    assert!(claims["iat"].is_u64());
}
