//! [`RequestContext`] — read-only metadata about who/where/when, plus the
//! cross-version-stable neutral types it carries.
//!
//! `RequestContext` is *pure metadata*. MRTR fields (`request_state`,
//! `input_responses`) live in the typed request body, not here (round-1 C2.3).
//! Version-specific negotiated capabilities are injected via [`Extensions`] by
//! the service-layer negotiation/legacy adapter rather than typed into core —
//! this keeps `turbomcp4-core` the bottom layer with no dependency on
//! `turbomcp4-protocol` (a deliberate refinement of PLAN.md §4.2, which named a
//! version-specific `ClientCapabilities` type that would invert the layering).

use crate::{CancellationToken, Identity, ProtocolVersion};
use alloc::boxed::Box;
use alloc::string::String;
use core::any::{Any, TypeId};
use core::fmt;
use hashbrown::HashMap;
use serde_json::{Map, Value};

/// Server/client implementation identity (`Implementation` in the spec).
///
/// Neutral-safe: evolves additively across versions. Unknown fields are
/// preserved in `extra` for forward compatibility.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Implementation {
    /// Programmatic name (e.g. `"my-server"`).
    pub name: String,
    /// Human-friendly title, if provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Version string.
    pub version: String,
    /// Any additional fields present on the wire (forward compatibility).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Implementation {
    /// Construct an [`Implementation`].
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: None,
            version: version.into(),
            extra: Map::new(),
        }
    }
}

/// MCP logging severity (`LoggingLevel`). Stable across versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Debug-level detail.
    Debug,
    /// Informational.
    Info,
    /// Normal but significant.
    Notice,
    /// Warning.
    Warning,
    /// Error.
    Error,
    /// Critical.
    Critical,
    /// Action must be taken immediately.
    Alert,
    /// System is unusable.
    Emergency,
}

/// W3C Trace Context, extracted from `_meta` (or HTTP headers on legacy).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct TraceContext {
    /// `traceparent` header value.
    pub traceparent: String,
    /// `tracestate` header value, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
    /// `baggage` header value, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baggage: Option<String>,
}

/// A tower-style type-map for ad-hoc, typed plumbing through the stack.
///
/// Used (among other things) to carry version-specific negotiated capabilities
/// from the negotiation/legacy layer down to handlers without coupling
/// `turbomcp4-core` to `turbomcp4-protocol`.
#[derive(Default)]
pub struct Extensions {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Extensions {
    /// Create an empty type-map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a value, returning the previous value of the same type, if any.
    pub fn insert<T: Any + Send + Sync>(&mut self, val: T) -> Option<T> {
        self.map
            .insert(TypeId::of::<T>(), Box::new(val))
            .and_then(|prev| prev.downcast::<T>().ok().map(|b| *b))
    }

    /// Get a shared reference to a value of type `T`, if present.
    #[must_use]
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get a mutable reference to a value of type `T`, if present.
    pub fn get_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|b| b.downcast_mut::<T>())
    }

    /// Remove and return the value of type `T`, if present.
    pub fn remove<T: Any + Send + Sync>(&mut self) -> Option<T> {
        self.map
            .remove(&TypeId::of::<T>())
            .and_then(|b| b.downcast::<T>().ok().map(|b| *b))
    }

    /// Number of stored values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extensions")
            .field("len", &self.map.len())
            .finish()
    }
}

impl Clone for Extensions {
    /// Type-maps of `dyn Any` cannot be deep-cloned; cloning yields an empty
    /// map. `RequestContext` is per-request and not expected to be cloned with
    /// its extensions intact; this exists only to keep `RequestContext: Clone`.
    fn clone(&self) -> Self {
        Self::new()
    }
}

/// Read-only metadata about a single request (PLAN.md §4.2).
///
/// `#[non_exhaustive]`: construct via [`RequestContext::new`] + the `with_*`
/// builders (the framework) or [`RequestContext::test_default`] (downstream
/// tests, behind the `test-util` feature).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RequestContext {
    /// Negotiated/declared protocol version for this request.
    pub protocol_version: ProtocolVersion,
    /// Client implementation identity, if known.
    pub client_info: Option<Implementation>,
    /// Raw advertised client capabilities (version-specific typed access is
    /// provided one layer up via per-RPC context types / [`Extensions`]).
    pub client_capabilities: Option<Value>,
    /// Requested logging level, if set.
    pub log_level: Option<LogLevel>,
    /// W3C trace context, if present.
    pub trace_context: Option<TraceContext>,
    /// Who made the request.
    pub identity: Identity,
    /// Per-request cancellation; always present, fresh per request.
    pub cancellation: CancellationToken,
    /// `_meta` keys not consumed by the framework (echoed on responses).
    pub propagated_meta: Map<String, Value>,
    /// Type-map for ad-hoc typed plumbing.
    pub extensions: Extensions,
}

impl RequestContext {
    /// Create a context for the given protocol version with default everything
    /// else (anonymous identity, fresh cancellation token, empty maps).
    #[must_use]
    pub fn new(protocol_version: ProtocolVersion) -> Self {
        Self {
            protocol_version,
            client_info: None,
            client_capabilities: None,
            log_level: None,
            trace_context: None,
            identity: Identity::Anonymous,
            cancellation: CancellationToken::new(),
            propagated_meta: Map::new(),
            extensions: Extensions::new(),
        }
    }

    /// Builder: set the identity.
    #[must_use]
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// Builder: set the client implementation identity.
    #[must_use]
    pub fn with_client_info(mut self, info: Implementation) -> Self {
        self.client_info = Some(info);
        self
    }

    /// Builder: set the trace context.
    #[must_use]
    pub fn with_trace_context(mut self, tc: TraceContext) -> Self {
        self.trace_context = Some(tc);
        self
    }

    /// Builder: set the propagated `_meta` map.
    #[must_use]
    pub fn with_propagated_meta(mut self, meta: Map<String, Value>) -> Self {
        self.propagated_meta = meta;
        self
    }

    /// A default context for downstream handler unit tests (round-3 SC-4).
    ///
    /// Available behind the `test-util` feature so that `#[non_exhaustive]`
    /// doesn't make `RequestContext` impossible to construct in tests.
    #[cfg(any(feature = "test-util", test))]
    #[must_use]
    pub fn test_default() -> Self {
        Self::new(ProtocolVersion::LATEST)
    }
}

impl Default for RequestContext {
    fn default() -> Self {
        Self::new(ProtocolVersion::LATEST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extensions_typed_roundtrip() {
        #[derive(Debug, PartialEq)]
        struct Tenant(u32);
        let mut ext = Extensions::new();
        assert!(ext.insert(Tenant(42)).is_none());
        assert_eq!(ext.get::<Tenant>(), Some(&Tenant(42)));
        assert_eq!(ext.remove::<Tenant>(), Some(Tenant(42)));
        assert!(ext.is_empty());
    }

    #[test]
    fn implementation_preserves_unknown_fields() {
        let json = json!({"name":"s","version":"1.0","websiteUrl":"https://x"});
        let imp: Implementation = serde_json::from_value(json).unwrap();
        assert_eq!(imp.name, "s");
        assert_eq!(imp.extra.get("websiteUrl").unwrap(), &json!("https://x"));
    }

    #[test]
    fn test_default_constructs() {
        let ctx = RequestContext::test_default();
        assert_eq!(ctx.protocol_version, ProtocolVersion::LATEST);
        assert!(!ctx.identity.is_authenticated());
        assert!(!ctx.cancellation.is_cancelled());
    }
}
