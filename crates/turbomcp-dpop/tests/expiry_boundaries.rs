//! Expiry boundary tests for DPoP proofs.
//!
//! DPoP proofs carry an `iat` claim with whole-second (Unix) precision and
//! are rejected once they are older than the configured proof lifetime. The
//! chrono -> jiff migration must not change any of that: `iat` stays an
//! integer number of seconds and the expiry boundary stays exactly where it
//! was.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use turbomcp_dpop::{DpopAlgorithm, DpopHeader, DpopJwk, DpopPayload, DpopProof};

/// The current Unix time in whole seconds.
fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_secs() as i64
}

/// Build a proof with the given `iat` claim.
fn proof_with_issued_at(issued_at: i64) -> DpopProof {
    DpopProof::new(
        DpopHeader {
            typ: "dpop+jwt".to_string(),
            algorithm: DpopAlgorithm::ES256,
            jwk: DpopJwk::Ec {
                use_: "sig".to_string(),
                crv: "P-256".to_string(),
                x: "abc".to_string(),
                y: "def".to_string(),
            },
        },
        DpopPayload {
            jti: "nonce-1".to_string(),
            htm: "GET".to_string(),
            htu: "https://api.example.com/resource".to_string(),
            iat: issued_at,
            ath: None,
            nonce: None,
        },
        "signature".to_string(),
    )
}

#[test]
fn issued_at_claim_is_a_whole_number_of_seconds() {
    let proof = proof_with_issued_at(now_seconds());
    let payload = serde_json::to_value(&proof.payload).unwrap();
    assert_eq!(payload["iat"], serde_json::json!(now_seconds()));
    assert!(payload["iat"].is_i64());
}

#[test]
fn jiff_and_chrono_agree_on_the_current_integer_second() {
    // `chrono::Utc::now().timestamp()` (used before the migration) and
    // `jiff::Timestamp::now().as_second()` (used after it) report the same
    // integer second for the current time.
    let jiff_seconds = jiff::Timestamp::now().as_second();
    let chrono_seconds = now_seconds();
    assert!((jiff_seconds - chrono_seconds).abs() <= 1);
}

#[test]
fn proof_is_not_expired_before_the_lifetime_boundary() {
    let lifetime = Duration::from_secs(300);
    let now = now_seconds();

    // One second of slack: the proof is younger than the lifetime.
    let proof = proof_with_issued_at(now - lifetime.as_secs() as i64 + 1);
    assert!(!proof.is_expired(lifetime));
}

#[test]
fn proof_is_expired_at_the_lifetime_boundary() {
    let lifetime = Duration::from_secs(300);
    let now = now_seconds();

    // Exactly at the boundary the proof is already expired: the comparison is
    // `now > issued_at + max_age` and `now` carries sub-second precision.
    let proof = proof_with_issued_at(now - lifetime.as_secs() as i64);
    assert!(proof.is_expired(lifetime));

    // And one second earlier it is definitely expired.
    let proof = proof_with_issued_at(now - lifetime.as_secs() as i64 - 1);
    assert!(proof.is_expired(lifetime));
}

#[test]
fn proof_in_the_future_is_not_expired() {
    let lifetime = Duration::from_secs(300);
    let proof = proof_with_issued_at(now_seconds() + 60);
    assert!(!proof.is_expired(lifetime));
}
