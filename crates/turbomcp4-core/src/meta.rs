//! `_meta` well-known keys and propagation policy (PLAN.md §13.2).
//!
//! The framework consumes a fixed set of keys; everything else is preserved in
//! [`crate::RequestContext::propagated_meta`] and echoed back on responses.
//! Extensions add keys under their reverse-DNS namespace.

use crate::ProtocolVersion;
use alloc::string::String;
use serde_json::{Map, Value};

/// Well-known `_meta` keys recognized by the framework.
pub mod keys {
    /// Per-request protocol version (draft stateless model). Verified present
    /// in `schema/draft/schema.ts:83`.
    pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
    /// Progress correlation token.
    pub const PROGRESS_TOKEN: &str = "progressToken";
    /// W3C Trace Context — traceparent (SEP-414; re-verify number).
    pub const TRACEPARENT: &str = "traceparent";
    /// W3C Trace Context — tracestate.
    pub const TRACESTATE: &str = "tracestate";
    /// W3C Baggage.
    pub const BAGGAGE: &str = "baggage";
    /// Subscription stream correlation id (draft `subscriptions/listen`).
    pub const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
}

/// Whether a `_meta` key is consumed by the framework (and therefore should not
/// be blindly propagated to responses without the framework's involvement).
#[must_use]
pub fn is_framework_key(key: &str) -> bool {
    matches!(
        key,
        keys::PROTOCOL_VERSION
            | keys::PROGRESS_TOKEN
            | keys::TRACEPARENT
            | keys::TRACESTATE
            | keys::BAGGAGE
            | keys::SUBSCRIPTION_ID
    )
}

/// Extract the per-request protocol version from a `_meta` map (draft model).
///
/// Returns `None` if the key is absent or not a string. Unrecognized version
/// strings parse to [`ProtocolVersion::Unknown`] rather than `None`.
#[must_use]
pub fn extract_protocol_version(meta: &Map<String, Value>) -> Option<ProtocolVersion> {
    meta.get(keys::PROTOCOL_VERSION)
        .and_then(Value::as_str)
        .map(ProtocolVersion::from_wire)
}

/// Partition a `_meta` map into (framework-consumed, propagated) halves.
///
/// The propagated half is what the framework preserves on the request context
/// and echoes to response `_meta` unless a handler overrides it.
#[must_use]
pub fn partition(meta: Map<String, Value>) -> (Map<String, Value>, Map<String, Value>) {
    let mut consumed = Map::new();
    let mut propagated = Map::new();
    for (k, v) in meta {
        if is_framework_key(&k) {
            consumed.insert(k, v);
        } else {
            propagated.insert(k, v);
        }
    }
    (consumed, propagated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_draft_version() {
        let mut meta = Map::new();
        meta.insert(keys::PROTOCOL_VERSION.into(), json!("DRAFT-2026-v1"));
        assert_eq!(
            extract_protocol_version(&meta),
            Some(ProtocolVersion::Draft2026V1)
        );
    }

    #[test]
    fn partition_preserves_user_keys_only() {
        let mut meta = Map::new();
        meta.insert(keys::TRACEPARENT.into(), json!("00-abc-def-01"));
        meta.insert("com.acme/tenant".into(), json!("t-42"));
        let (consumed, propagated) = partition(meta);
        assert!(consumed.contains_key(keys::TRACEPARENT));
        assert!(propagated.contains_key("com.acme/tenant"));
        assert_eq!(propagated.len(), 1);
    }
}
