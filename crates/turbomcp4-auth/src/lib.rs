//! TurboMCP v4 auth — the OAuth 2.1 **resource-server** half.
//!
//! An MCP HTTP server is an OAuth 2.1 resource server: it validates the bearer
//! tokens clients present and tells clients where to get them. This crate
//! provides that validation, plus the RFC 9728 metadata document and the
//! `WWW-Authenticate` challenges. It does **not** implement OAuth *flows*
//! (authorization-code, PKCE, dynamic client registration) — those are a
//! client concern and land with `turbomcp4-client`.
//!
//! Auth is HTTP-transport-level (the MCP authorization spec): the token rides
//! the `Authorization` header, never `_meta`, and stdio has no auth. So the
//! seam is [`turbomcp4_service::HttpAuthenticator`], which [`ResourceServer`]
//! implements; wire it into the HTTP transport with
//! `HttpConfig::with_authenticator`.
//!
//! ```no_run
//! use std::sync::Arc;
//! use turbomcp4_auth::{JwtValidator, ResourceMetadata, ResourceServer, StaticJwks};
//!
//! # fn jwks_json() -> &'static str { "{\"keys\":[]}" }
//! let jwks = StaticJwks::from_json(jwks_json()).unwrap();
//! let validator = JwtValidator::new(jwks, "https://mcp.example.com", "https://auth.example.com");
//! let metadata = ResourceMetadata::new(
//!     "https://mcp.example.com",
//!     ["https://auth.example.com"],
//! );
//! let resource_server = Arc::new(ResourceServer::new(
//!     validator,
//!     metadata,
//!     "https://mcp.example.com/.well-known/oauth-protected-resource",
//! ));
//! // HttpConfig::new().with_authenticator(resource_server)
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod jwks;
mod metadata;
mod resource_server;
mod validator;

pub use error::AuthError;
pub use jwks::{JwkSource, StaticJwks};
pub use metadata::ResourceMetadata;
pub use resource_server::ResourceServer;
pub use validator::{AuthPrincipal, BearerValidator, JwtValidator};

#[cfg(feature = "http-jwks")]
pub use jwks::HttpJwks;

/// Re-exported from `jsonwebtoken` for configuring [`JwtValidator`].
pub use jsonwebtoken::Algorithm;
