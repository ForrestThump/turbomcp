//! # Stateful server (v4)
//!
//! Shared mutable state across requests. The server struct holds an
//! `Arc<RwLock<…>>`; because `#[server]` only requires the type to be `Clone`,
//! every cloned handler shares the same state through the `Arc`. Named counters
//! are incremented, read, and reset concurrently.
//!
//! Run with: `cargo run -p turbomcp4 --example stateful`

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use turbomcp4::prelude::*;

#[derive(Clone)]
struct CounterServer {
    /// Counters keyed by name, shared across every request.
    counters: Arc<RwLock<HashMap<String, i64>>>,
}

#[server(name = "counter", version = "1.0.0")]
impl CounterServer {
    /// Increment a counter by name, returning its new value.
    #[tool(description = "Increment a counter by name")]
    async fn increment(&self, name: String) -> McpResult<String> {
        let mut counters = self.counters.write().await;
        let counter = counters.entry(name).or_insert(0);
        *counter += 1;
        Ok(counter.to_string())
    }

    /// Get a counter's current value (0 if it has never been incremented).
    #[tool(description = "Get a counter's current value")]
    async fn get(&self, name: String) -> McpResult<String> {
        let counters = self.counters.read().await;
        Ok(counters.get(&name).copied().unwrap_or(0).to_string())
    }

    /// Reset a counter, removing it from the map.
    #[tool(description = "Reset a counter")]
    async fn reset(&self, name: String) -> McpResult<String> {
        let mut counters = self.counters.write().await;
        counters.remove(&name);
        Ok(format!("counter '{name}' reset"))
    }

    /// List every counter and its value.
    #[tool(description = "List all counters")]
    async fn list(&self) -> McpResult<String> {
        let counters = self.counters.read().await;
        if counters.is_empty() {
            return Ok("(no counters yet)".to_string());
        }
        let mut lines: Vec<String> = counters.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        lines.sort();
        Ok(lines.join(", "))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    let server = CounterServer {
        counters: Arc::new(RwLock::new(HashMap::new())),
    };
    server.run_stdio().await
}
