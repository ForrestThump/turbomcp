//! # Structured output (v4)
//!
//! Returning typed, structured data from a tool with [`Json<T>`]. The value is
//! placed in the result's `structuredContent` (what programmatic clients read
//! and validate) and a JSON text mirror is added to `content` (what
//! text-only clients and models read) — the spec's recommended shape for typed
//! results. Because the `#[server]` macro sees the `-> Json<Stats>` return, it
//! also generates the tool's `outputSchema` from `Stats`.
//!
//! `T` must be `serde::Serialize` (for the value) and `schemars::JsonSchema`
//! (for the output schema). `schemars` is re-exported as `turbomcp4::schemars`.
//!
//! Run with: `cargo run -p turbomcp4 --example structured_output`

use schemars::JsonSchema;
use serde::Serialize;
use turbomcp4::prelude::*;

/// Summary statistics over a list of numbers.
#[derive(Serialize, JsonSchema)]
struct Stats {
    count: usize,
    sum: f64,
    mean: f64,
    min: f64,
    max: f64,
}

#[derive(Clone)]
struct Analyzer;

#[server(name = "analyzer", version = "1.0.0")]
impl Analyzer {
    /// Compute summary statistics for a non-empty list of numbers.
    #[tool(description = "Summary statistics for a list of numbers")]
    async fn analyze(&self, numbers: Vec<f64>) -> McpResult<Json<Stats>> {
        if numbers.is_empty() {
            return Err(McpError::invalid_params("numbers must be non-empty"));
        }
        let count = numbers.len();
        let sum: f64 = numbers.iter().sum();
        let mean = sum / count as f64;
        let min = numbers.iter().copied().fold(f64::INFINITY, f64::min);
        let max = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Ok(Json(Stats {
            count,
            sum,
            mean,
            min,
            max,
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    Analyzer.run_stdio().await
}
