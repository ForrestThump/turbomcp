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

use turbomcp4_core::{McpError, McpResult};
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

/// A tool that does work but returns nothing → a successful, empty-content
/// result (the spec permits an empty `content` array).
impl IntoCallToolResult for () {
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
        Ok(neutral::CallToolResult::default())
    }
}

/// Scalar returns become a single text block of their `Display` form. This is a
/// *concrete* set, not a blanket `impl<T: Display>`: a blanket would overlap the
/// `String`/`&str` impls (no specialization) and silently accept any `Display`
/// type. Structured/object data should use `Json` (→ `structuredContent`)
/// instead of a stringified scalar.
macro_rules! scalar_tool_result {
    ($($t:ty),* $(,)?) => {$(
        impl IntoCallToolResult for $t {
            fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
                Ok(neutral::CallToolResult::text(self.to_string()))
            }
        }
    )*};
}
scalar_tool_result!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64, bool,
);

/// A fallible tool: an error becomes an `is_error` result (spec convention) —
/// its text is the tool-failure message. The one exception is the MRTR abort
/// sentinel, which must keep propagating so the dispatcher can answer an
/// `InputRequiredResult` instead of a failed tool call.
impl<T> IntoCallToolResult for McpResult<T>
where
    T: IntoCallToolResult,
{
    fn into_call_tool_result(self) -> McpResult<neutral::CallToolResult> {
        match self {
            Ok(v) => v.into_call_tool_result(),
            Err(e @ McpError::InputRequired) => Err(e),
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
