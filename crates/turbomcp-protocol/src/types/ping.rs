//! Connection testing types
//!
//! This module contains types for MCP ping functionality,
//! allowing connection health checking between clients and servers.

use serde::{Deserialize, Serialize};

/// Ping request parameters.
///
/// Per MCP 2025-11-25 (`schema.ts:578-581`), `PingRequest.params?` only carries
/// optional `_meta`. The `data` field is a **TurboMCP-specific extension** used
/// by the WebSocket reconnect probe (`turbomcp-websocket`) to echo arbitrary
/// payload bytes for round-trip verification. Spec-strict peers will tolerate
/// the extra field on receive (extra params are allowed) but will not echo it
/// back. Callers writing portable code should leave `data: None`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PingParams {
    /// Optional data to echo back. **Non-spec TurboMCP extension** —
    /// see the type-level docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Ping request wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingRequest {
    /// Ping parameters
    #[serde(flatten)]
    pub params: PingParams,
}

/// Ping result (echoes back the data)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingResult {
    /// Echoed data from the request
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Optional metadata per the current MCP specification
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub _meta: Option<serde_json::Value>,
}

impl PingResult {
    /// Create a new ping result
    pub fn new(data: Option<serde_json::Value>) -> Self {
        Self { data, _meta: None }
    }

    /// Create a ping result with no data
    pub fn empty() -> Self {
        Self::new(None)
    }

    /// Add metadata to this result
    pub fn with_meta(mut self, meta: serde_json::Value) -> Self {
        self._meta = Some(meta);
        self
    }
}
