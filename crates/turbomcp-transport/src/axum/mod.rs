//! Axum Integration Layer for TurboMCP
//!
//! **Deprecated since 3.2.0.** This subtree predates the MCP 2025-11-25 Streamable
//! HTTP rework and lacks `Mcp-Session-Id` lifecycle, `Last-Event-ID` resumption, and
//! the unified `/mcp` method-multiplexed endpoint. New code should serve over
//! `turbomcp_server::transport::http`, which is spec-compliant. The subtree will be
//! removed in a future major release. See `CHANGELOG.md` for migration guidance.
//!
//! The compile-time `#[deprecated]` attributes live on the public re-exports in
//! the parent crate's `lib.rs` (`AxumMcpExt`, `McpAppState`, `McpServerConfig`,
//! `McpService`). Consumers reaching in via `turbomcp_transport::axum::…` paths
//! still hit those re-exports through the documented public surface; this avoids
//! cascading the deprecation into every test and use-site inside the subtree
//! itself, which `#![allow(deprecated)]` cannot silence across file boundaries.
//!
//! This module provides seamless integration with Axum routers, enabling the
//! "bring your own server" philosophy while providing opinionated defaults for
//! rapid development.
//!
//! NOTE: This entire module is only compiled when feature="http" is enabled.
//! See lib.rs for the module-level feature gate.

// Silence deprecation warnings within this subtree itself. The deprecation
// targets external consumers; internal references back through `crate::axum::*`
// would otherwise generate noise during the deprecation window.
#![allow(deprecated)]

#[cfg(feature = "auth")]
pub mod auth_router;
pub mod config;
pub mod handlers;
pub mod middleware;
pub mod query;
pub mod router;
pub mod service;
pub mod types;
pub mod websocket_bidirectional;
pub mod websocket_factory;

#[cfg(test)]
pub mod tests;

// Re-export main public types (avoiding glob conflicts).
//
// The four documented public-API types — `AxumMcpExt`, `McpAppState`,
// `McpServerConfig`, `McpService` — are `#[deprecated]` at their source
// definitions, so any path that resolves to them fires the migration warning.
// `#[allow(deprecated)]` here keeps the re-export sites themselves quiet.
#[allow(deprecated)]
pub use config::{
    AuthConfig, CorsConfig, Environment, McpServerConfig, RateLimitConfig, SecurityConfig,
    TlsConfig,
};
pub use handlers::{
    SessionInfo, capabilities_handler, health_handler, json_rpc_handler, metrics_handler,
    sse_handler, websocket_handler,
};
#[allow(deprecated)]
pub use router::AxumMcpExt;
#[allow(deprecated)]
pub use service::{McpAppState, McpService};
pub use types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, SseQuery, WebSocketQuery};
pub use websocket_bidirectional::{WebSocketDispatcher, handle_response_correlation, is_response};
pub use websocket_factory::{
    HandlerFactory, WebSocketFactoryState, websocket_handler_with_factory,
};
