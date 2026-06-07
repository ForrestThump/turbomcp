//! # Hello World Server (v4)
//!
//! The simplest possible MCP server — one tool, minimal code — and a direct port
//! of the v3 `hello_world.rs` example, demonstrating that the v4 macro surface is
//! source-compatible for the common case.
//!
//! Run with: `cargo run -p turbomcp4 --example hello_world`

use turbomcp4::prelude::*;

#[derive(Clone)]
struct HelloServer;

#[server(name = "hello", version = "1.0.0")]
impl HelloServer {
    /// Say hello to someone.
    #[tool(description = "Say hello to someone")]
    async fn hello(&self, name: String) -> McpResult<String> {
        Ok(format!("Hello, {name}!"))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    HelloServer.run_stdio().await
}
