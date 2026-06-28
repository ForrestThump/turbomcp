//! # Elicitation (v4)
//!
//! A tool that asks the user for confirmation before acting, via
//! `ctx.client.elicit(…)`. The same handler works on **both protocol
//! versions** without changes:
//!
//! - `DRAFT-2026-v1` (MRTR, SEP-2322): the first call answers
//!   `InputRequiredResult`; the client gathers the user's answer and retries
//!   the call with `inputResponses`, re-running the handler from the top —
//!   keep elicit keys stable and pre-elicit work idempotent.
//! - `2025-11-25`: the call blocks while a real `elicitation/create` request
//!   goes to the client inline; no re-execution happens.
//!
//! Run with: `cargo run -p turbomcp --example elicitation`

use serde_json::json;
use turbomcp::prelude::*;

#[derive(Clone)]
struct FileManager;

#[server(name = "file-manager", version = "1.0.0")]
impl FileManager {
    /// Delete a path — but only after the user confirms.
    #[tool(description = "Delete a path after user confirmation")]
    async fn delete(&self, ctx: &CallToolContext, path: String) -> McpResult<String> {
        let outcome = ctx
            .client
            .elicit(
                "confirm_delete", // stable key: MRTR re-execution finds the answer here
                neutral::ElicitParams::new(
                    format!("Really delete {path}?"),
                    json!({
                        "type": "object",
                        "properties": { "ok": { "type": "boolean", "title": "Confirm" } },
                        "required": ["ok"],
                    }),
                ),
            )
            .await?;

        if outcome.accepted() && outcome.content.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            // Side effects belong AFTER the elicit: on the draft this line
            // runs exactly once, in the retry execution.
            Ok(format!("deleted {path}"))
        } else {
            Ok(format!("kept {path}"))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    FileManager.run_stdio().await
}
