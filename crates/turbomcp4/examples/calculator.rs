//! # Calculator (v4)
//!
//! A handful of arithmetic tools on one `#[server]` — the "more than one tool"
//! step up from `hello_world`. Shows infallible tools returning a bare scalar
//! (`-> f64`, wrapped as a text content block) alongside a fallible one
//! (`-> McpResult<f64>`, where the error becomes a `CallToolResult` with
//! `isError: true` rather than a transport error).
//!
//! Tools may return `String`/`&str`, any numeric or `bool` scalar, `()` (empty
//! success), `Json<T>` (structured output), or a `neutral::CallToolResult` —
//! each optionally wrapped in `McpResult<_>`.
//!
//! Run with: `cargo run -p turbomcp4 --example calculator`
//!
//! Drive it from a shell:
//! ```bash
//! printf '%s\n' \
//!   '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"cli","version":"1.0"},"capabilities":{}}}' \
//!   '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
//!   '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"add","arguments":{"a":5,"b":3}}}' \
//! | cargo run -p turbomcp4 --example calculator
//! ```

use turbomcp4::prelude::*;

#[derive(Clone)]
struct Calculator;

#[server(name = "calculator", version = "1.0.0")]
impl Calculator {
    /// Add two numbers together.
    #[tool(description = "Add two numbers")]
    async fn add(&self, a: f64, b: f64) -> f64 {
        a + b
    }

    /// Subtract `b` from `a`.
    #[tool(description = "Subtract b from a")]
    async fn subtract(&self, a: f64, b: f64) -> f64 {
        a - b
    }

    /// Multiply two numbers.
    #[tool(description = "Multiply two numbers")]
    async fn multiply(&self, a: f64, b: f64) -> f64 {
        a * b
    }

    /// Divide `a` by `b` — fails on division by zero.
    #[tool(description = "Divide a by b")]
    async fn divide(&self, a: f64, b: f64) -> McpResult<f64> {
        if b == 0.0 {
            return Err(McpError::invalid_params("cannot divide by zero"));
        }
        Ok(a / b)
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    Calculator.run_stdio().await
}
