//! MCP service trait definition
//!
//! This module defines the core MCP service trait that implementations
//! must provide to handle MCP protocol requests.

// See `mod.rs` — internal subtree references silenced; deprecation fires for
// external consumers via the source-level `#[deprecated]` attributes.
#![allow(deprecated)]

use std::future::Future;
use std::pin::Pin;

use crate::tower::SessionInfo;
use turbomcp_protocol::PROTOCOL_VERSION;
use turbomcp_protocol::Result as McpResult;

/// Core MCP service trait
///
/// **Deprecated since 3.2.0.** This subtree predates the MCP 2025-11-25 Streamable
/// HTTP rework. Use `turbomcp_server::transport::http` for spec-compliant serving.
///
/// Implementations of this trait provide the business logic for handling
/// MCP protocol requests. The trait is designed to be object-safe to
/// allow for dynamic dispatch.
#[deprecated(
    since = "3.2.0",
    note = "Use `turbomcp_server::transport::http` for spec-compliant Streamable HTTP \
            (MCP 2025-11-25). This subtree will be removed in a future major release."
)]
pub trait McpService: Send + Sync + 'static {
    /// Process an MCP request and return a response
    ///
    /// # Arguments
    ///
    /// * `request` - The JSON-RPC request payload
    /// * `session` - Session information for the current request
    ///
    /// # Returns
    ///
    /// Returns the JSON response or an error if processing fails.
    fn process_request(
        &self,
        request: serde_json::Value,
        session: &SessionInfo,
    ) -> Pin<Box<dyn Future<Output = McpResult<serde_json::Value>> + Send + '_>>;

    /// Get service capabilities
    ///
    /// Returns the capabilities that this service supports,
    /// following the MCP protocol specification.
    fn get_capabilities(&self) -> serde_json::Value {
        serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "capabilities": {
                "tools": true,
                "resources": true,
                "prompts": true,
                "logging": true
            }
        })
    }
}

impl std::fmt::Debug for dyn McpService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpService")
            .field("capabilities", &self.get_capabilities())
            .finish()
    }
}
