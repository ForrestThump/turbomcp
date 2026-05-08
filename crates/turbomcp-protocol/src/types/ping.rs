//! Connection testing types
//!
//! This module contains types for MCP ping functionality,
//! allowing connection health checking between clients and servers.

use serde::{Deserialize, Serialize};

/// Ping request parameters.
///
/// MCP ping requests have no ping-specific parameters. The only supported
/// parameter field is the common `_meta` object.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PingParams {
    /// Optional metadata per the current MCP specification.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub _meta: Option<serde_json::Value>,
}

impl PingParams {
    /// Create ping params with metadata.
    pub fn with_meta(meta: serde_json::Value) -> Self {
        Self { _meta: Some(meta) }
    }
}

/// Ping request wrapper.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PingRequest {
    /// Ping parameters.
    #[serde(flatten)]
    pub params: PingParams,
}

/// Ping result.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PingResult {
    /// Optional metadata per the current MCP specification.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub _meta: Option<serde_json::Value>,
}

impl PingResult {
    /// Create a ping result with optional metadata.
    pub fn new(meta: Option<serde_json::Value>) -> Self {
        Self { _meta: meta }
    }

    /// Create an empty ping result.
    pub fn empty() -> Self {
        Self::new(None)
    }

    /// Add metadata to this result.
    pub fn with_meta(mut self, meta: serde_json::Value) -> Self {
        self._meta = Some(meta);
        self
    }
}
