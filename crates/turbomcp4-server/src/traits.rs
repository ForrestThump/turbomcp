//! The user-facing server traits: one required core trait plus one trait per
//! capability. A server *is* what it implements — the [`MethodRouter`] derives
//! advertised capabilities from which of these are present, so there is no way
//! for declared capabilities to drift from actual handlers.
//!
//! Capability methods use native async-fn-in-trait (RPITIT) and speak
//! [`turbomcp4_protocol::neutral`] types, never wire types. That is what lets a
//! second wire version (Phase 5) be added without touching a single handler.
//!
//! [`MethodRouter`]: crate::MethodRouter

use core::future::Future;

use turbomcp4_core::{Implementation, McpResult, ProtocolVersion};
use turbomcp4_protocol::neutral;

use crate::context::{CallToolContext, ListToolsContext};

/// The required server trait. Every server implements this; capability traits
/// (`WithTools`, …) are added à la carte.
///
/// `Clone + Send + Sync + 'static`: the dispatcher clones the server per request
/// and the handler runs on a spawned task. Keep the server cheap to clone (wrap
/// shared state in `Arc`).
pub trait McpServerCore: Clone + Send + Sync + 'static {
    /// Identity of this server implementation (name, version, optional title).
    fn server_info(&self) -> Implementation;

    /// Protocol versions this server accepts. Defaults to
    /// [`ProtocolVersion::SUPPORTED`] (the current stable set).
    fn supported_versions(&self) -> &'static [ProtocolVersion] {
        ProtocolVersion::SUPPORTED
    }

    /// Natural-language guidance returned to clients in discovery, to help an
    /// LLM use the server effectively. `None` by default.
    fn instructions(&self) -> Option<String> {
        None
    }
}

/// Implement to serve tools (`tools/list`, `tools/call`).
pub trait WithTools: McpServerCore {
    /// Enumerate available tools.
    fn list_tools(
        &self,
        ctx: &ListToolsContext,
    ) -> impl Future<Output = McpResult<neutral::ListToolsResult>> + Send;

    /// Invoke a tool. Per spec, tool-level failure is reported via
    /// [`neutral::CallToolResult::error`] (so the model can self-correct), not
    /// as a JSON-RPC error.
    fn call_tool(
        &self,
        ctx: &CallToolContext,
        params: neutral::CallToolParams,
    ) -> impl Future<Output = McpResult<neutral::CallToolResult>> + Send;
}
