//! Authentication middleware for WASM MCP servers.
//!
//! Provides a wrapper that adds authentication to any MCP server.
//!
//! # Example
//!
//! ```ignore
//! use turbomcp_wasm::wasm_server::{McpServer, WithAuth};
//! use turbomcp_wasm::auth::{CloudflareAccessAuthenticator};
//!
//! let server = McpServer::builder("my-server", "1.0.0")
//!     .tool("hello", "Say hello", handler)
//!     .build();
//!
//! // Wrap with Cloudflare Access authentication
//! let auth = CloudflareAccessAuthenticator::new("my-team", "my-aud");
//! let protected_server = WithAuth::new(server, auth);
//!
//! // Handle requests (authentication happens automatically)
//! protected_server.handle(request).await
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use turbomcp_core::auth::{
    AuthError, Authenticator, CredentialExtractor, HeaderExtractor, Principal,
};
use worker::{Request, Response};

use super::server::McpServer;
use super::types::{JsonRpcResponse, error_codes};

/// Authentication-enabled MCP server wrapper.
///
/// Wraps an [`McpServer`] with an [`Authenticator`] to require authentication
/// for all requests. The authenticated [`Principal`] is stored and can be
/// accessed during request handling.
///
/// # Example
///
/// ```ignore
/// use turbomcp_wasm::wasm_server::{McpServer, WithAuth};
/// use turbomcp_wasm::auth::CloudflareAccessAuthenticator;
///
/// let server = McpServer::builder("my-server", "1.0.0")
///     .tool("hello", "Say hello", handler)
///     .build();
///
/// let auth = CloudflareAccessAuthenticator::new("my-team", "my-aud");
/// let protected = WithAuth::new(server, auth);
///
/// // In your fetch handler:
/// protected.handle(request).await
/// ```
pub struct WithAuth<A, E = HeaderExtractor>
where
    A: Authenticator<Error = AuthError> + Clone + 'static,
    E: CredentialExtractor + 'static,
{
    server: McpServer,
    authenticator: A,
    extractor: E,
    /// Current request's principal (set during authentication)
    current_principal: Rc<RefCell<Option<Principal>>>,
    /// Skip authentication for certain methods
    skip_auth_methods: Vec<String>,
}

impl<A> WithAuth<A, HeaderExtractor>
where
    A: Authenticator<Error = AuthError> + Clone + 'static,
{
    /// Create a new authenticated server wrapper.
    ///
    /// Uses the default [`HeaderExtractor`] to extract credentials from
    /// the Authorization header.
    pub fn new(server: McpServer, authenticator: A) -> Self {
        Self {
            server,
            authenticator,
            extractor: HeaderExtractor,
            current_principal: Rc::new(RefCell::new(None)),
            skip_auth_methods: vec![
                "initialize".to_string(),
                "notifications/initialized".to_string(),
                "ping".to_string(),
            ],
        }
    }
}

impl<A, E> WithAuth<A, E>
where
    A: Authenticator<Error = AuthError> + Clone + 'static,
    E: CredentialExtractor + 'static,
{
    /// Create with a custom credential extractor.
    pub fn with_extractor(server: McpServer, authenticator: A, extractor: E) -> Self {
        Self {
            server,
            authenticator,
            extractor,
            current_principal: Rc::new(RefCell::new(None)),
            skip_auth_methods: vec![
                "initialize".to_string(),
                "notifications/initialized".to_string(),
                "ping".to_string(),
            ],
        }
    }

    /// Configure methods that don't require authentication.
    ///
    /// By default, `initialize`, `notifications/initialized`, and `ping`
    /// are allowed without authentication.
    pub fn skip_auth_for(mut self, methods: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.skip_auth_methods = methods.into_iter().map(Into::into).collect();
        self
    }

    /// Add a method to the skip list.
    pub fn also_skip_auth_for(mut self, method: impl Into<String>) -> Self {
        self.skip_auth_methods.push(method.into());
        self
    }

    /// Get the current request's principal.
    ///
    /// Returns `None` if no request is being processed or if the request
    /// was to a method that doesn't require authentication.
    pub fn principal(&self) -> Option<Principal> {
        self.current_principal.borrow().clone()
    }

    /// Handle an incoming request with authentication.
    ///
    /// Extracts credentials, validates them, and then delegates to the
    /// underlying server. Returns HTTP 401 if authentication fails.
    ///
    /// When no credential is supplied, the request is allowed through only
    /// if its JSON-RPC method appears in `skip_auth_methods` (default:
    /// `initialize`, `notifications/initialized`, `ping`). Any other method
    /// is rejected with HTTP 401 + `WWW-Authenticate: Bearer`.
    pub async fn handle(&self, req: Request) -> worker::Result<Response> {
        // SECURITY: Extract Origin header early for CORS responses.
        // We echo this back instead of using wildcard "*".
        let request_origin = req.headers().get("origin").ok().flatten();
        let origin_ref = request_origin.as_deref();

        // Handle CORS preflight (no auth needed)
        if req.method() == worker::Method::Options {
            return self.server.handle(req).await;
        }

        // Extract credentials from request
        let credential = {
            let headers = req.headers();
            self.extractor
                .extract(|name| headers.get(name).ok().flatten())
        };

        // Authenticate if we have credentials
        if let Some(cred) = credential {
            match self.authenticator.authenticate(&cred).await {
                Ok(principal) => {
                    *self.current_principal.borrow_mut() = Some(principal);
                }
                Err(e) => {
                    // Clear any previous principal
                    *self.current_principal.borrow_mut() = None;
                    return self.auth_error_response(&e, origin_ref);
                }
            }
        } else {
            // No credentials: only allow methods on the skip list.
            // Worker bodies are one-shot, so clone the request to peek at
            // the JSON-RPC method without consuming the original.
            let method_name = match req.clone() {
                Ok(mut peeker) => match peeker.text().await {
                    Ok(body) => parse_jsonrpc_method(&body),
                    Err(_) => None,
                },
                Err(_) => None,
            };
            let allowed = method_name
                .as_deref()
                .map(|m| self.skip_auth_methods.iter().any(|s| s == m))
                .unwrap_or(false);
            if !allowed {
                return self.unauthenticated_method_response(method_name.as_deref(), origin_ref);
            }
        }

        // Delegate to the underlying server
        let response = self.server.handle(req).await;

        // Clear principal after request
        *self.current_principal.borrow_mut() = None;

        response
    }

    /// Build the 401 response for an unauthenticated call to a protected method.
    fn unauthenticated_method_response(
        &self,
        method: Option<&str>,
        request_origin: Option<&str>,
    ) -> worker::Result<Response> {
        let headers = worker::Headers::new();
        let origin = request_origin.unwrap_or("*");
        let _ = headers.set("Access-Control-Allow-Origin", origin);
        if request_origin.is_some() {
            let _ = headers.set("Vary", "Origin");
        }
        let _ = headers.set("Content-Type", "application/json");
        let _ = headers.set("WWW-Authenticate", "Bearer");

        let detail = match method {
            Some(m) => format!("Authentication required for method '{m}'"),
            None => "Authentication required".to_string(),
        };
        let response = JsonRpcResponse::error(
            None,
            error_codes::INTERNAL_ERROR - 5, // -32008 for authentication errors
            detail,
        );

        let json = serde_json::to_string(&response)
            .unwrap_or_else(|_| r#"{"error":"Authentication required"}"#.to_string());

        Response::error(json, 401).map(|r| r.with_headers(headers))
    }

    /// Create an authentication error response.
    ///
    /// SECURITY: Echoes the request Origin header instead of using wildcard `*`.
    fn auth_error_response(
        &self,
        error: &AuthError,
        request_origin: Option<&str>,
    ) -> worker::Result<Response> {
        let headers = worker::Headers::new();
        // SECURITY: Echo the request origin instead of using wildcard.
        let origin = request_origin.unwrap_or("*");
        let _ = headers.set("Access-Control-Allow-Origin", origin);
        if request_origin.is_some() {
            let _ = headers.set("Vary", "Origin");
        }
        let _ = headers.set("Content-Type", "application/json");
        let _ = headers.set("WWW-Authenticate", "Bearer");

        let response = JsonRpcResponse::error(
            None,
            error_codes::INTERNAL_ERROR - 5, // -32008 for authentication errors
            error.to_string(),
        );

        let json = serde_json::to_string(&response)
            .unwrap_or_else(|_| r#"{"error":"Authentication failed"}"#.to_string());

        Response::error(json, 401).map(|r| r.with_headers(headers))
    }
}

/// Best-effort extraction of the JSON-RPC `method` field from a request body.
///
/// Returns `None` for invalid JSON, missing field, non-string method, or batch
/// requests (MCP 2025-11-25 deprecates batch). Callers treat `None` as
/// "cannot determine method — reject for safety" when no credentials are
/// present.
fn parse_jsonrpc_method(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("method")?.as_str().map(str::to_owned)
}

/// Extension trait for adding authentication to [`McpServer`].
pub trait AuthExt {
    /// Wrap this server with authentication.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use turbomcp_wasm::wasm_server::{McpServer, AuthExt};
    /// use turbomcp_wasm::auth::CloudflareAccessAuthenticator;
    ///
    /// let server = McpServer::builder("my-server", "1.0.0")
    ///     .tool("hello", "Say hello", handler)
    ///     .build()
    ///     .with_auth(CloudflareAccessAuthenticator::new("team", "aud"));
    /// ```
    fn with_auth<A>(self, authenticator: A) -> WithAuth<A, HeaderExtractor>
    where
        A: Authenticator<Error = AuthError> + Clone + 'static;

    /// Wrap this server with authentication using a custom extractor.
    fn with_auth_extractor<A, E>(self, authenticator: A, extractor: E) -> WithAuth<A, E>
    where
        A: Authenticator<Error = AuthError> + Clone + 'static,
        E: CredentialExtractor + 'static;
}

impl AuthExt for McpServer {
    fn with_auth<A>(self, authenticator: A) -> WithAuth<A, HeaderExtractor>
    where
        A: Authenticator<Error = AuthError> + Clone + 'static,
    {
        WithAuth::new(self, authenticator)
    }

    fn with_auth_extractor<A, E>(self, authenticator: A, extractor: E) -> WithAuth<A, E>
    where
        A: Authenticator<Error = AuthError> + Clone + 'static,
        E: CredentialExtractor + 'static,
    {
        WithAuth::with_extractor(self, authenticator, extractor)
    }
}

#[cfg(test)]
mod tests {
    // Tests would require wasm-bindgen-test for full coverage
    // These are compile-time checks for the API

    use super::*;

    #[allow(clippy::extra_unused_type_parameters)]
    fn _assert_with_auth_compiles<A: Authenticator<Error = AuthError> + Clone + 'static>() {
        // Verify the type can be constructed
        fn _needs_with_auth<
            A: Authenticator<Error = AuthError> + Clone + 'static,
            E: CredentialExtractor,
        >(
            _: WithAuth<A, E>,
        ) {
        }
    }
}
