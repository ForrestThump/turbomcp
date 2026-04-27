//! Client configuration types and utilities
//!
//! This module contains configuration structures for MCP client initialization
//! results. The `ConnectionConfig` type lives in the crate root (`crate::ConnectionConfig`).

use turbomcp_protocol::types::ServerCapabilities;

/// Result of client initialization containing server information
#[derive(Debug, Clone)]
pub struct InitializeResult {
    /// Information about the server
    pub server_info: turbomcp_protocol::types::Implementation,

    /// Capabilities supported by the server
    pub server_capabilities: ServerCapabilities,
}
