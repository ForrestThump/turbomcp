//! # turbomcp4
//!
//! The TurboMCP v4 SDK facade: a single crate that re-exports the layered
//! workspace crates and the `#[server]` / `#[tool]` / `#[resource]` / `#[prompt]`
//! macros, plus a [`prelude`] for the common imports.
//!
//! ```ignore
//! use turbomcp4::prelude::*;
//!
//! #[derive(Clone)]
//! struct Hello;
//!
//! #[server(name = "hello", version = "1.0.0")]
//! impl Hello {
//!     /// Say hello to someone.
//!     #[tool]
//!     async fn hello(&self, name: String) -> McpResult<String> {
//!         Ok(format!("Hello, {name}!"))
//!     }
//! }
//! ```
#![forbid(unsafe_code)]

// ---- foundation -------------------------------------------------------------

pub use turbomcp4_core::{
    Implementation, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, McpError,
    McpResult, ProtocolVersion, RequestContext, RequestId,
};

/// Version-stable, handler-facing types (the surface user handlers speak).
pub use turbomcp4_protocol::neutral;

// ---- service seam + codec ---------------------------------------------------

pub use turbomcp4_codec::{Codec, CodecError, DefaultCodec, SerdeJsonCodec};
pub use turbomcp4_service::{
    CancellationToken, McpService, ProtocolError, ServeConfig, Transport, serve, serve_with,
};

// ---- server -----------------------------------------------------------------

pub use turbomcp4_server::{
    CallToolContext, CompleteContext, GetPromptContext, IntoCallToolResult, IntoGetPromptResult,
    IntoReadResourceResult, IntoServerBuilder, LegacySessionAdapter, ListPromptsContext,
    ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, McpServerCore,
    MethodRouter, ReadResourceContext, ServerBuilder, SessionState, SessionStore,
    VersionDispatcher, WithCompletions, WithPrompts, WithResources, WithTools,
};

// ---- transports -------------------------------------------------------------

pub use turbomcp4_transport_stdio::{serve_stdio, serve_stdio_with, stdio};

/// Streamable HTTP transport (axum 0.8). Enable with the `http` feature.
///
/// Serve a macro server over HTTP by building its dispatcher and handing it to
/// [`serve_http`](http::serve_http):
///
/// ```ignore
/// use turbomcp4::http::{serve_http, HttpConfig};
/// use turbomcp4::IntoServerBuilder;
///
/// // `.into_server()` resolves to the macro-generated inherent method, so the
/// // server's capabilities are pre-registered.
/// let service = MyServer.into_server().build();
/// serve_http("127.0.0.1:8080".parse()?, service, HttpConfig::new()).await?;
/// ```
#[cfg(feature = "http")]
pub mod http {
    pub use turbomcp4_transport_http::{HttpConfig, HttpError, router, serve_http};
}

// ---- macros -----------------------------------------------------------------

pub use turbomcp4_macros::{mcp_header, prompt, resource, server, tool};

/// Support items referenced by `#[server]`-generated code. **Not** a stable API
/// — do not depend on it directly; it exists only so generated code has a single
/// rooted path (`::turbomcp4::__macros::…`) for its dependencies.
#[doc(hidden)]
pub mod __macros {
    pub use schemars;
    pub use serde;
    pub use serde_json;

    pub use turbomcp4_core::{McpError, McpResult};
    pub use turbomcp4_protocol::neutral;
    pub use turbomcp4_server::__macro_support::{mark_mcp_header, normalize_input_schema};
}

/// The common imports for building a server.
pub mod prelude {
    pub use crate::neutral;
    pub use turbomcp4_core::{Implementation, McpError, McpResult, RequestContext};
    pub use turbomcp4_server::{
        CallToolContext, CompleteContext, GetPromptContext, IntoServerBuilder, ListPromptsContext,
        ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, McpServerCore,
        ServerBuilder, WithCompletions, WithPrompts, WithResources, WithTools,
    };
    pub use turbomcp4_transport_stdio::serve_stdio;

    pub use turbomcp4_macros::{mcp_header, prompt, resource, server, tool};
}
