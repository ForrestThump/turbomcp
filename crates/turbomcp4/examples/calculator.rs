//! # Calculator (v4)
//!
//! A handful of arithmetic tools on one `#[server]` — the "more than one tool"
//! step up from `hello_world`. Shows infallible tools (`-> String`, auto-wrapped
//! as text content) alongside a fallible one (`-> McpResult<String>`, where the
//! error becomes a `CallToolResult` with `isError: true` rather than a transport
//! error).
//!
//! Note (v4 vs v3): v3 let tools return bare scalars (`-> i64`) and stringified
//! them for you. v4 tools return `String` / `McpResult<String>` /
//! `neutral::CallToolResult`, so we format the result ourselves — explicit about
//! what text the client sees.
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
    async fn add(&self, a: f64, b: f64) -> String {
        format!("{}", a + b)
    }

    /// Subtract `b` from `a`.
    #[tool(description = "Subtract b from a")]
    async fn subtract(&self, a: f64, b: f64) -> String {
        format!("{}", a - b)
    }

    /// Multiply two numbers.
    #[tool(description = "Multiply two numbers")]
    async fn multiply(&self, a: f64, b: f64) -> String {
        format!("{}", a * b)
    }

    /// Divide `a` by `b` — fails on division by zero.
    #[tool(description = "Divide a by b")]
    async fn divide(&self, a: f64, b: f64) -> McpResult<String> {
        if b == 0.0 {
            return Err(McpError::invalid_params("cannot divide by zero"));
        }
        Ok(format!("{}", a / b))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    Calculator.run_stdio().await
}
