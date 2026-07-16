//! End-to-end exercise of the `#[server]` / `#[tool]` / `#[resource]` /
//! `#[prompt]` / `#[mcp_header]` macros: a macro-defined server is driven
//! through the real `VersionDispatcher`, and its generated tool schemas are
//! snapshot-tested with `insta`.

use tower::{Service, ServiceExt};
use turbomcp::prelude::*;
use turbomcp::{JsonRpcMessage, JsonRpcRequest};

#[derive(Clone)]
struct Demo;

#[server(
    name = "demo",
    version = "1.0.0",
    title = "Demo Server",
    instructions = "A demo server exercising every macro."
)]
impl Demo {
    /// Say hello to someone.
    #[tool(description = "Say hello to someone")]
    async fn hello(&self, name: String) -> McpResult<String> {
        Ok(format!("Hello, {name}!"))
    }

    /// Add two numbers (the second is optional).
    #[tool]
    async fn add(&self, ctx: &CallToolContext, a: f64, b: Option<f64>) -> McpResult<String> {
        let _ = ctx; // context is available; unused here
        Ok(format!("{}", a + b.unwrap_or(0.0)))
    }

    /// Run a query, mirroring `region` into a request header.
    #[tool]
    async fn query(
        &self,
        #[mcp_header] region: String,
        #[description("The SQL to run")] sql: String,
    ) -> McpResult<String> {
        Ok(format!("{region}: {sql}"))
    }

    /// The application configuration.
    #[resource("config://app")]
    async fn app_config(&self) -> McpResult<String> {
        Ok(r#"{"debug":false}"#.to_string())
    }

    /// Summarize some text.
    #[prompt]
    async fn summarize(&self, text: String) -> McpResult<String> {
        Ok(format!("Please summarize:\n\n{text}"))
    }
}

fn draft_meta() -> serde_json::Value {
    serde_json::json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28" })
}

async fn call(req: JsonRpcRequest) -> serde_json::Value {
    let mut svc = Demo.into_server().build();
    let JsonRpcMessage::Response(r) = svc
        .ready()
        .await
        .unwrap()
        .call(req.into())
        .await
        .unwrap()
        .expect("a response")
    else {
        panic!("expected a response")
    };
    if let Some(err) = r.error {
        serde_json::json!({ "error": { "code": err.code, "message": err.message } })
    } else {
        r.result.expect("a result")
    }
}

#[tokio::test]
async fn discover_derives_capabilities_from_impls() {
    let result = call(JsonRpcRequest::new(1, "server/discover", None)).await;
    // The draft carries the server identity in the result `_meta` (the
    // dedicated `DiscoverResult.serverInfo` field was removed upstream).
    let server_info = &result["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(server_info["name"], "demo");
    assert_eq!(server_info["title"], "Demo Server");
    assert_eq!(
        result["instructions"],
        "A demo server exercising every macro."
    );
    let caps = &result["capabilities"];
    assert!(caps["tools"].is_object());
    assert!(caps["resources"].is_object());
    assert!(caps["prompts"].is_object());
    // No #[completion] handlers → no completions capability.
    assert!(caps.get("completions").is_none());
}

#[tokio::test]
async fn tool_schemas_are_generated() {
    let result = call(JsonRpcRequest::new(
        2,
        "tools/list",
        Some(serde_json::json!({ "_meta": draft_meta() })),
    ))
    .await;
    // Stable ordering for the snapshot.
    let mut tools = result["tools"].as_array().unwrap().clone();
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    insta::assert_json_snapshot!(tools);
}

#[tokio::test]
async fn tool_call_validates_then_invokes() {
    // Valid call.
    let ok = call(JsonRpcRequest::new(
        3,
        "tools/call",
        Some(serde_json::json!({
            "name": "add",
            "arguments": { "a": 2.0, "b": 40.0 },
            "_meta": draft_meta(),
        })),
    ))
    .await;
    assert_eq!(ok["content"][0]["text"], "42");
    assert_eq!(ok["isError"], false);

    // Missing required `a` → is_error result (NOT a -32602), per spec.
    let bad = call(JsonRpcRequest::new(
        4,
        "tools/call",
        Some(serde_json::json!({
            "name": "add",
            "arguments": { "b": 1.0 },
            "_meta": draft_meta(),
        })),
    ))
    .await;
    assert_eq!(bad["isError"], true);
    assert!(
        bad["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("invalid arguments")
    );
}

#[tokio::test]
async fn hello_tool_uses_doc_and_explicit_description() {
    let result = call(JsonRpcRequest::new(
        5,
        "tools/call",
        Some(serde_json::json!({
            "name": "hello",
            "arguments": { "name": "Ada" },
            "_meta": draft_meta(),
        })),
    ))
    .await;
    assert_eq!(result["content"][0]["text"], "Hello, Ada!");
}

#[tokio::test]
async fn mcp_header_param_is_marked_in_schema() {
    let result = call(JsonRpcRequest::new(
        6,
        "tools/list",
        Some(serde_json::json!({ "_meta": draft_meta() })),
    ))
    .await;
    let query = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "query")
        .unwrap();
    assert_eq!(
        query["inputSchema"]["properties"]["region"]["x-mcp-header"],
        true
    );
    assert_eq!(
        query["inputSchema"]["properties"]["sql"]["description"],
        "The SQL to run"
    );
}

#[tokio::test]
async fn resource_read_routes_by_uri() {
    let result = call(JsonRpcRequest::new(
        7,
        "resources/read",
        Some(serde_json::json!({ "uri": "config://app", "_meta": draft_meta() })),
    ))
    .await;
    assert_eq!(result["contents"][0]["uri"], "config://app");
    assert_eq!(result["contents"][0]["text"], r#"{"debug":false}"#);

    let missing = call(JsonRpcRequest::new(
        8,
        "resources/read",
        Some(serde_json::json!({ "uri": "config://nope", "_meta": draft_meta() })),
    ))
    .await;
    assert!(missing["error"].is_object());
}

#[tokio::test]
async fn prompt_get_extracts_arguments() {
    let result = call(JsonRpcRequest::new(
        9,
        "prompts/get",
        Some(serde_json::json!({
            "name": "summarize",
            "arguments": { "text": "the quick brown fox" },
            "_meta": draft_meta(),
        })),
    ))
    .await;
    assert_eq!(result["messages"][0]["role"], "user");
    assert!(
        result["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("the quick brown fox")
    );
}
