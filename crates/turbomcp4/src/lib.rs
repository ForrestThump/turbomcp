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
    Implementation, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, LogLevel,
    McpError, McpResult, ProtocolVersion, RequestContext, RequestId,
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
    CallToolContext, ClientHandle, CompleteContext, GetPromptContext, IntoCallToolResult,
    IntoGetPromptResult, IntoReadResourceResult, IntoServerBuilder, LegacySessionAdapter,
    ListPromptsContext, ListResourceTemplatesContext, ListResourcesContext, ListToolsContext,
    LogSender, McpServerCore, MethodRouter, ProgressReporter, ReadResourceContext, ServerBuilder,
    ServerNotifier, SessionState, SessionStore, VersionDispatcher, WithCompletions, WithPrompts,
    WithResources, WithTools,
};

// ---- transports -------------------------------------------------------------

pub use turbomcp4_transport_stdio::{serve_stdio, serve_stdio_with, stdio};

/// Streamable HTTP transport (axum 0.8). Enable with the `http` feature.
///
/// The one-liner is [`ServeHttp::run_http`](http::ServeHttp::run_http) on a
/// builder — it builds the dispatcher, wires session termination (`DELETE`)
/// automatically, and serves:
///
/// ```ignore
/// use turbomcp4::prelude::*;
/// use turbomcp4::http::{HttpConfig, ServeHttp};
///
/// MyServer.into_server().run_http("127.0.0.1:8080".parse()?, HttpConfig::new()).await?;
/// ```
///
/// For full control (e.g. wrapping the dispatcher in RPC middleware like the
/// telemetry [`TraceContextLayer`](crate::telemetry::TraceContextLayer)), build
/// the service yourself and call [`serve_http`](http::serve_http):
///
/// ```ignore
/// use tower::Layer;
/// let service = TraceContextLayer::new().layer(MyServer.into_server().build());
/// serve_http(addr, service, HttpConfig::new()).await?;
/// ```
#[cfg(feature = "http")]
pub mod http {
    use std::net::SocketAddr;
    use std::sync::Arc;

    pub use turbomcp4_service::SessionTerminator;
    pub use turbomcp4_transport_http::{HttpConfig, HttpError, router, serve_http};

    use turbomcp4_server::{McpServerCore, ServerBuilder};

    /// One-call HTTP serving for a [`ServerBuilder`] (the value
    /// `MyServer.into_server()` produces).
    pub trait ServeHttp {
        /// Build this server's dispatcher and serve it over Streamable HTTP on
        /// `addr` until `config`'s shutdown token fires.
        ///
        /// Session termination (`DELETE`) is wired automatically from the built
        /// dispatcher, so the endpoint honors client-initiated termination by
        /// default. To compose RPC middleware first, build the dispatcher
        /// yourself and call [`serve_http`] instead.
        fn run_http(
            self,
            addr: SocketAddr,
            config: HttpConfig,
        ) -> impl std::future::Future<Output = Result<(), HttpError>> + Send;
    }

    impl<S> ServeHttp for ServerBuilder<S>
    where
        S: McpServerCore + Clone + Send + Sync + 'static,
    {
        async fn run_http(self, addr: SocketAddr, config: HttpConfig) -> Result<(), HttpError> {
            let dispatcher = self.build();
            let config = config.with_session_terminator(Arc::new(dispatcher.session_terminator()));
            serve_http(addr, dispatcher, config).await
        }
    }
}

/// OAuth 2.1 resource-server auth: bearer-token validation + RFC 9728 metadata.
/// Enable with the `auth` feature, then protect an HTTP endpoint with
/// [`HttpConfig::with_authenticator`](http::HttpConfig::with_authenticator).
#[cfg(feature = "auth")]
pub use turbomcp4_auth as auth;

/// The HTTP authentication seam (implemented by [`auth::ResourceServer`]).
#[cfg(feature = "http")]
pub use turbomcp4_service::{AuthDecision, HttpAuthenticator};

/// The HTTP rate-limiting seam + the in-process `governor`-backed default.
/// Apply with [`HttpConfig::with_rate_limiter`](http::HttpConfig::with_rate_limiter).
#[cfg(feature = "http")]
pub use turbomcp4_service::{GovernorRateLimiter, RateKey, RateLimiter};

/// OpenTelemetry observability: the [`TraceContextLayer`](telemetry::TraceContextLayer)
/// (W3C trace continuation over `_meta` + PII-safe identity spans) and an
/// optional OTLP export pipeline. Enable with the `telemetry` feature.
#[cfg(feature = "telemetry")]
pub use turbomcp4_telemetry as telemetry;

/// The MCP client: [`client::ClientBuilder`] runs the handshake + version
/// negotiation, then [`client::Client`] speaks the typed [`neutral`] API.
/// Enable with the `client` feature.
#[cfg(feature = "client")]
pub use turbomcp4_client as client;

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
    pub use turbomcp4_core::{Implementation, LogLevel, McpError, McpResult, RequestContext};
    pub use turbomcp4_server::{
        CallToolContext, CompleteContext, GetPromptContext, IntoServerBuilder, ListPromptsContext,
        ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, McpServerCore,
        ServerBuilder, WithCompletions, WithPrompts, WithResources, WithTools,
    };
    pub use turbomcp4_transport_stdio::serve_stdio;

    /// The HTTP one-liner `builder.run_http(addr, config)` (feature `http`).
    #[cfg(feature = "http")]
    pub use crate::http::ServeHttp;

    pub use turbomcp4_macros::{mcp_header, prompt, resource, server, tool};
}
