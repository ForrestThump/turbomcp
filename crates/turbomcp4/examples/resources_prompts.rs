//! # Resources & prompts (v4)
//!
//! The non-tool half of the handler surface. `#[resource("uri")]` exposes
//! readable content at a fixed URI (`resources/list` + `resources/read`);
//! `#[prompt]` exposes a reusable prompt template whose arguments come straight
//! from the function signature (`prompts/list` + `prompts/get`). Capabilities are
//! derived from which markers are present — declaring a `#[resource]` is what
//! advertises the `resources` capability, so they can't drift from the impl.
//!
//! (v4 today serves fixed-URI resources; RFC 6570 templated URIs like
//! `file://{path}` are a tracked follow-up.)
//!
//! Run with: `cargo run -p turbomcp4 --example resources_prompts`

use turbomcp4::prelude::*;

#[derive(Clone)]
struct Docs;

#[server(name = "docs", version = "1.0.0")]
impl Docs {
    /// The application's (static) configuration, as JSON.
    #[resource("config://app")]
    async fn app_config(&self) -> McpResult<String> {
        Ok(r#"{"name":"docs","debug":false,"max_items":100}"#.to_string())
    }

    /// A short README served as plain text.
    #[resource("docs://readme")]
    async fn readme(&self) -> McpResult<String> {
        Ok(
            "# Docs server\n\nA tiny MCP server exposing a config resource \
            and two prompt templates."
                .to_string(),
        )
    }

    /// Build a prompt asking the model to summarize some text.
    #[prompt]
    async fn summarize(&self, text: String) -> McpResult<String> {
        Ok(format!(
            "Please summarize the following text concisely:\n\n{text}"
        ))
    }

    /// Build a prompt asking the model to translate text into a target language.
    #[prompt]
    async fn translate(&self, text: String, target_language: String) -> McpResult<String> {
        Ok(format!(
            "Translate the following text into {target_language}. \
             Reply with only the translation.\n\n{text}"
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    Docs.run_stdio().await
}
