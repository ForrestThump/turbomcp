//! Return-type conversions for macro-generated handlers.
//!
//! A `#[tool]` / `#[resource]` / `#[prompt]` method may return a plain `String`,
//! the neutral result type directly, or a fallible `McpResult<_>`. These traits
//! let the `#[server]` macro emit one uniform `.into_*()` call regardless, while
//! enforcing the spec's differing error semantics per method family:
//!
//! - **Tools**: a handler error becomes a `CallToolResult { is_error: true }`
//!   (the model sees and self-corrects — PLAN §4.11), *not* a JSON-RPC error.
//! - **Resources / prompts**: a handler error *propagates* as a JSON-RPC error,
//!   since `resources/read` and `prompts/get` have no `is_error` channel.

use core::fmt::Display;

use turbomcp4_core::McpResult;
use turbomcp4_protocol::neutral;

/// Convert a `#[tool]` return value into a [`neutral::CallToolResult`].
pub trait IntoCallToolResult {
    /// Perform the conversion.
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult>;
}

impl IntoCallToolResult for neutral::CallToolResult {
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
        Ok(self)
    }
}

impl IntoCallToolResult for String {
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
        Ok(neutral::CallToolResult::text(self))
    }
}

impl IntoCallToolResult for &str {
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
        Ok(neutral::CallToolResult::text(self))
    }
}

/// A fallible tool: an error becomes an `is_error` result (spec convention), so
/// any `Display` error type is accepted — its text is the tool-failure message.
impl<T, E> IntoCallToolResult for Result<T, E>
where
    T: IntoCallToolResult,
    E: Display,
{
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
        match self {
            Ok(v) => v.into_call_tool_result(),
            Err(e) => Ok(neutral::CallToolResult::error(e.to_string())),
        }
    }
}

/// Convert a `#[resource]` return value into a [`neutral::ReadResourceResult`],
/// using the resolved `uri` when wrapping bare text.
pub trait IntoReadResourceResult {
    /// Perform the conversion. `uri` is the resource being read.
    fn into_read_resource_result(self, uri: &str) -> McpResult<neutral::ReadResourceResult>;
}

impl IntoReadResourceResult for neutral::ReadResourceResult {
    fn into_read_resource_result(self, _uri: &str) -> McpResult<neutral::ReadResourceResult> {
        Ok(self)
    }
}

impl IntoReadResourceResult for String {
    fn into_read_resource_result(self, uri: &str) -> McpResult<neutral::ReadResourceResult> {
        Ok(neutral::ReadResourceResult::text(uri, self))
    }
}

impl IntoReadResourceResult for &str {
    fn into_read_resource_result(self, uri: &str) -> McpResult<neutral::ReadResourceResult> {
        Ok(neutral::ReadResourceResult::text(uri, self))
    }
}

/// A fallible resource read: the error *propagates* (no `is_error` channel).
impl<T> IntoReadResourceResult for McpResult<T>
where
    T: IntoReadResourceResult,
{
    fn into_read_resource_result(self, uri: &str) -> McpResult<neutral::ReadResourceResult> {
        self.and_then(|v| v.into_read_resource_result(uri))
    }
}

/// Convert a `#[prompt]` return value into a [`neutral::GetPromptResult`]. A bare
/// string becomes a single user message.
pub trait IntoGetPromptResult {
    /// Perform the conversion.
    fn into_get_prompt_result(self) -> McpResult<neutral::GetPromptResult>;
}

impl IntoGetPromptResult for neutral::GetPromptResult {
    fn into_get_prompt_result(self) -> McpResult<neutral::GetPromptResult> {
        Ok(self)
    }
}

impl IntoGetPromptResult for String {
    fn into_get_prompt_result(self) -> McpResult<neutral::GetPromptResult> {
        Ok(neutral::GetPromptResult::new(vec![
            neutral::PromptMessage::user_text(self),
        ]))
    }
}

impl IntoGetPromptResult for &str {
    fn into_get_prompt_result(self) -> McpResult<neutral::GetPromptResult> {
        Ok(neutral::GetPromptResult::new(vec![
            neutral::PromptMessage::user_text(self),
        ]))
    }
}

/// A fallible prompt render: the error *propagates*.
impl<T> IntoGetPromptResult for McpResult<T>
where
    T: IntoGetPromptResult,
{
    fn into_get_prompt_result(self) -> McpResult<neutral::GetPromptResult> {
        self.and_then(IntoGetPromptResult::into_get_prompt_result)
    }
}
