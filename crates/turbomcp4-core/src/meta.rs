//! `_meta` well-known keys and propagation policy (PLAN.md §13.2).
//!
//! The framework consumes a fixed set of keys; everything else is preserved in
//! [`crate::RequestContext::propagated_meta`] and echoed back on responses.
//! Extensions add keys under their reverse-DNS namespace.

use crate::{JsonRpcMessage, ProtocolVersion};
use alloc::string::{String, ToString};
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
    /// Per-request client implementation info (draft stateless model).
    pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
    /// Per-request client capabilities (draft stateless model). Gates which
    /// MRTR input requests a server may send (SEP-2322 MUST).
    pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
}

/// Internal `_meta` keys: the in-process side-channel a transport (or session
/// adapter) uses to hand the dispatcher facts only it knows — never part of the
/// wire protocol. Transports **must** strip these from inbound messages (see
/// [`sanitize_inbound`]) before injecting their own, so a client cannot forge
/// them; the dispatcher consumes them, so they never echo back out.
pub mod internal {
    /// The session this message belongs to (legacy `2025-11-25` stateful path).
    /// Injected by the HTTP transport (from the `Mcp-Session-Id` header) or the
    /// stdio `LegacySessionAdapter` (per-connection).
    pub const SESSION_ID: &str = "io.turbomcp.internal/sessionId";

    /// The connection this message arrived on. Injected by the serve driver
    /// (one id per `serve` call), scoping in-flight request cancellation —
    /// `notifications/cancelled` can only reach requests from the same
    /// connection. HTTP deliberately injects none: there, closing the response
    /// stream is the cancellation signal (transports spec §Cancellation).
    pub const CONNECTION_ID: &str = "io.turbomcp.internal/connectionId";

    /// Whether `key` is in the internal (in-process only) namespace.
    #[must_use]
    pub fn is_internal_key(key: &str) -> bool {
        key.starts_with("io.turbomcp.internal/")
    }
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
            | keys::CLIENT_INFO
            | keys::CLIENT_CAPABILITIES
    ) || internal::is_internal_key(key)
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

/// Insert `key: value` into a request's or notification's `params._meta`,
/// creating `params` and `_meta` as needed. Responses are left untouched, as
/// are (already-invalid) non-object `params`.
///
/// This is how transports assert per-message facts (session id, protocol
/// version) toward the dispatcher without changing the service seam.
pub fn set_request_meta(msg: &mut JsonRpcMessage, key: &str, value: Value) {
    let params = match msg {
        JsonRpcMessage::Request(r) => &mut r.params,
        JsonRpcMessage::Notification(n) => &mut n.params,
        JsonRpcMessage::Response(_) => return,
    };
    let params = params.get_or_insert_with(|| Value::Object(Map::new()));
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    let meta = obj
        .entry("_meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(key.to_string(), value);
    }
}

/// Strip all [`internal`] keys from an inbound message's `params._meta`.
///
/// Transports **must** call this on every message received from a client
/// before injecting their own internal keys — otherwise a client could forge
/// a session id or other in-process assertion.
pub fn sanitize_inbound(msg: &mut JsonRpcMessage) {
    let params = match msg {
        JsonRpcMessage::Request(r) => r.params.as_mut(),
        JsonRpcMessage::Notification(n) => n.params.as_mut(),
        JsonRpcMessage::Response(_) => None,
    };
    if let Some(meta) = params
        .and_then(Value::as_object_mut)
        .and_then(|p| p.get_mut("_meta"))
        .and_then(Value::as_object_mut)
    {
        meta.retain(|k, _| !internal::is_internal_key(k));
    }
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
        meta.insert(internal::SESSION_ID.into(), json!("s-1"));
        let (consumed, propagated) = partition(meta);
        assert!(consumed.contains_key(keys::TRACEPARENT));
        assert!(consumed.contains_key(internal::SESSION_ID));
        assert!(propagated.contains_key("com.acme/tenant"));
        assert_eq!(propagated.len(), 1);
    }

    #[test]
    fn set_request_meta_creates_params_and_meta() {
        use crate::JsonRpcRequest;
        let mut msg: JsonRpcMessage = JsonRpcRequest::new(1, "tools/list", None).into();
        set_request_meta(&mut msg, internal::SESSION_ID, json!("s-1"));
        set_request_meta(&mut msg, keys::PROTOCOL_VERSION, json!("2025-11-25"));
        let JsonRpcMessage::Request(r) = &msg else {
            unreachable!()
        };
        let meta = &r.params.as_ref().unwrap()["_meta"];
        assert_eq!(meta[internal::SESSION_ID], "s-1");
        assert_eq!(meta[keys::PROTOCOL_VERSION], "2025-11-25");
    }

    #[test]
    fn sanitize_strips_only_internal_keys() {
        use crate::JsonRpcRequest;
        let params = json!({
            "name": "echo",
            "_meta": {
                internal::SESSION_ID: "forged",
                "com.acme/tenant": "t-42",
            }
        });
        let mut msg: JsonRpcMessage = JsonRpcRequest::new(1, "tools/call", Some(params)).into();
        sanitize_inbound(&mut msg);
        let JsonRpcMessage::Request(r) = &msg else {
            unreachable!()
        };
        let meta = r.params.as_ref().unwrap()["_meta"].as_object().unwrap();
        assert!(!meta.contains_key(internal::SESSION_ID));
        assert!(meta.contains_key("com.acme/tenant"));
    }
}
