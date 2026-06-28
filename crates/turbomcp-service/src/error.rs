//! The service-layer error type and the canonical `McpError → JsonRpcError`
//! mapping used everywhere a user error must become a wire response.

use turbomcp_codec::CodecError;
use turbomcp_core::{JsonRpcError, JsonRpcResponse, McpError, RequestId};

/// Errors at the service/transport boundary — *not* normal protocol responses.
///
/// A user handler returning `Err(McpError)` is **not** a `ProtocolError`: it
/// becomes a JSON-RPC error response inside the `Ok` arm of the service (see
/// [`mcp_to_jsonrpc_error`]). `ProtocolError` is reserved for the conditions a
/// well-formed request can't itself produce: malformed frames, version
/// mismatch, a dead transport, shutdown.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// A frame could not be parsed (JSON-RPC `-32700`).
    #[error("parse error: {0}")]
    Parse(String),
    /// The request's protocol version is absent or unsupported (`-32004`).
    #[error("unsupported protocol version (requested {requested:?}; supported {supported:?})")]
    UnsupportedVersion {
        /// The version the client asked for, if any.
        requested: Option<String>,
        /// The versions this server accepts.
        supported: Vec<String>,
    },
    /// A required capability for the requested method is not available (`-32003`).
    #[error("missing required capability: {0}")]
    MissingCapability(String),
    /// The request referenced a session this server does not know — expired,
    /// evicted, or never created. Per the `2025-11-25` Streamable HTTP spec the
    /// HTTP transport answers this with `404 Not Found`, prompting the client
    /// to re-`initialize`.
    #[error("unknown session: {0}")]
    UnknownSession(String),
    /// The underlying transport failed (connection closed, I/O error).
    #[error("transport error: {0}")]
    Transport(String),
    /// The server is draining and will not accept new work.
    #[error("server is shutting down")]
    ServerShuttingDown,
    /// An unexpected internal failure (`-32603`).
    #[error("internal error: {0}")]
    Internal(String),
}

impl ProtocolError {
    /// The JSON-RPC error code for this condition.
    #[must_use]
    pub fn jsonrpc_code(&self) -> i32 {
        match self {
            Self::Parse(_) => -32700,
            Self::UnsupportedVersion { .. } => -32004,
            Self::MissingCapability(_) => -32003,
            Self::UnknownSession(_) => -32002,
            Self::Transport(_) | Self::ServerShuttingDown => -32001,
            Self::Internal(_) => -32603,
        }
    }

    /// Render this error as a JSON-RPC error response for `id`.
    ///
    /// Used when the error is still answerable on the wire (e.g. version
    /// mismatch on a request). Pure transport death has no response.
    #[must_use]
    pub fn into_response(self, id: RequestId) -> JsonRpcResponse {
        let code = self.jsonrpc_code();
        JsonRpcResponse::error(
            id,
            JsonRpcError {
                code,
                message: self.to_string(),
                data: None,
            },
        )
    }
}

impl From<CodecError> for ProtocolError {
    fn from(e: CodecError) -> Self {
        // Both encode and decode failures surface as parse errors at the
        // protocol boundary — the frame could not be turned into/from a value.
        ProtocolError::Parse(e.to_string())
    }
}

/// Convert a user [`McpError`] into a JSON-RPC error object using the single
/// canonical code mapping (PLAN §4.10). The dispatcher wraps this in an `Ok`
/// error *response*; it is never a [`ProtocolError`].
///
/// Note: [`McpError::ToolExecutionFailed`] has no protocol-level code — it is
/// surfaced as `CallToolResult { isError: true }`, handled upstream, and must
/// not reach this function.
#[must_use]
pub fn mcp_to_jsonrpc_error(err: &McpError) -> JsonRpcError {
    JsonRpcError {
        code: err.jsonrpc_code(),
        message: err.to_string(),
        data: None,
    }
}
