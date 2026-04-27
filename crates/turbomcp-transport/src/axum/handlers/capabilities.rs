//! Capabilities endpoint handler for MCP server capabilities

// See `mod.rs` — internal subtree references silenced; deprecation fires for
// external consumers via the source-level `#[deprecated]` attributes.
#![allow(deprecated)]

use axum::{Json, extract::State};

use crate::axum::service::McpAppState;

/// Capabilities handler - returns MCP server capabilities
pub async fn capabilities_handler(State(app_state): State<McpAppState>) -> Json<serde_json::Value> {
    Json(app_state.get_capabilities())
}
