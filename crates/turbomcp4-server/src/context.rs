//! Per-RPC context types.
//!
//! Each handler method receives a *typed* context that wraps the shared
//! [`RequestContext`] and conditionally exposes capabilities only valid for that
//! method. The load-bearing example (PLAN §4.4.1): `tools/call` may return an
//! `InputRequiredResult` (MRTR), so its context will carry a `ClientHandle`;
//! `tools/list` may not, so its context never will — calling `ctx.client` from
//! a `list_tools` handler is a *type error*, not a runtime check.
//!
//! The MRTR `client` handle and `request_state` accessor land in Phase 6; these
//! structs are `#[non_exhaustive]` so adding those fields then is non-breaking.

use turbomcp4_core::RequestContext;

/// Context for `tools/list`. Carries only the shared request metadata — no
/// client handle, because list operations cannot elicit from the client.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ListToolsContext {
    /// Shared per-request metadata.
    pub base: RequestContext,
}

impl ListToolsContext {
    /// Wrap a [`RequestContext`].
    #[must_use]
    pub fn new(base: RequestContext) -> Self {
        Self { base }
    }
}

/// Context for `tools/call`. In Phase 6 this gains the MRTR `ClientHandle` and a
/// `request_state` accessor; today it carries the shared request metadata.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CallToolContext {
    /// Shared per-request metadata.
    pub base: RequestContext,
}

impl CallToolContext {
    /// Wrap a [`RequestContext`].
    #[must_use]
    pub fn new(base: RequestContext) -> Self {
        Self { base }
    }
}
