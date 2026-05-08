use turbomcp::prelude::*;

#[derive(Clone)]
struct AuditServer;

#[server(name = "audit-server", version = "1.0.0")]
impl AuditServer {
    #[tool]
    async fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }

    #[resource("file://{name}")]
    async fn get_test(&self, name: String, _ctx: &RequestContext) -> Result<String, McpError> {
        Ok(format!("Content for {}", name))
    }
}

#[tokio::test]
async fn test_v3_server_compilation_and_execution() {
    let server = AuditServer;

    // Check metadata
    let info = server.server_info();
    assert_eq!(info.name, "audit-server");
    assert_eq!(info.version, "1.0.0");

    // Check tools
    let tools = server.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "add");

    // Templated resources are exposed through resources/templates/list, not as
    // concrete resources/list entries.
    let resources = server.list_resources();
    assert!(resources.is_empty());
    let templates = server.list_resource_templates();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].name, "get_test");
    assert_eq!(templates[0].uri_template, "file://{name}");

    // Test tool execution
    let ctx = RequestContext::stdio();
    let args = serde_json::json!({ "a": 10, "b": 20 });
    let result = server.call_tool("add", args, &ctx).await.unwrap();
    // ToolResult content should be text "30"
    // The implementation of IntoToolResult for i32 probably creates a TextContent
    // Let's verify the structure
    assert!(!result.is_error());

    // Test resource execution
    let result = server
        .read_resource("file://something", &ctx)
        .await
        .unwrap();
    assert_eq!(result.contents.len(), 1);
}

// Regression guard: MCP spec 2025-11-25 explicitly permits custom URI schemes,
// so `#[resource("apple-doc://{topic}")]` must dispatch through the macro-
// generated `read_resource` without being rejected by the scheme denylist.
#[derive(Clone)]
struct CustomSchemeServer;

#[server(name = "custom-scheme-server", version = "1.0.0")]
impl CustomSchemeServer {
    #[resource("apple-doc://{topic}")]
    async fn apple_doc(&self, topic: String, _ctx: &RequestContext) -> McpResult<String> {
        Ok(format!("apple-doc content for {topic}"))
    }

    #[resource("notion://{page}")]
    async fn notion(&self, page: String, _ctx: &RequestContext) -> McpResult<String> {
        Ok(format!("notion page {page}"))
    }
}

#[tokio::test]
async fn custom_uri_schemes_reach_registered_handlers() {
    let server = CustomSchemeServer;
    let ctx = RequestContext::stdio();

    let result = server
        .read_resource("apple-doc://swift/StringProtocol", &ctx)
        .await
        .expect("custom apple-doc:// scheme must reach its handler");
    assert_eq!(result.contents.len(), 1);

    let result = server
        .read_resource("notion://workspace-page-abc123", &ctx)
        .await
        .expect("custom notion:// scheme must reach its handler");
    assert_eq!(result.contents.len(), 1);
}

// SEP-973 / MCP 2025-11-25 surface fields plumbed through #[tool], #[resource],
// #[prompt]: title, icons, ToolAnnotations hints, and outputSchema.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct GreetingOut {
    greeting: String,
}

#[derive(Clone)]
struct AnnotatedServer;

#[server(name = "annotated", version = "1.0.0")]
impl AnnotatedServer {
    /// Greet a user
    #[tool(
        title = "Greet",
        icons = ["https://example.com/wave.svg"],
        read_only = true,
        idempotent = true,
        output_schema = GreetingOut,
    )]
    async fn greet(&self, name: String) -> String {
        format!("hi {name}")
    }

    #[resource(
        "config://app",
        title = "App config",
        icons = ["https://example.com/cog.png"],
        mime_type = "application/json"
    )]
    async fn config(&self, _uri: String, _ctx: &RequestContext) -> McpResult<String> {
        Ok("{}".into())
    }

    #[prompt(title = "Summarize", icons = ["https://example.com/doc.svg"])]
    async fn summarize(&self, text: String, _ctx: &RequestContext) -> McpResult<PromptResult> {
        Ok(PromptResult::user(format!("Summarize: {text}")))
    }
}

#[test]
fn tool_attributes_emit_2025_11_25_metadata() {
    let server = AnnotatedServer;

    let tools = server.list_tools();
    let tool = tools
        .iter()
        .find(|t| t.name == "greet")
        .expect("greet tool");
    assert_eq!(tool.title.as_deref(), Some("Greet"));
    let icons = tool.icons.as_ref().expect("icons populated");
    assert_eq!(icons.len(), 1);
    assert_eq!(icons[0].src, "https://example.com/wave.svg");

    let ann = tool.annotations.as_ref().expect("annotations populated");
    assert_eq!(ann.read_only_hint, Some(true));
    assert_eq!(ann.idempotent_hint, Some(true));
    assert_eq!(ann.destructive_hint, None);
    assert_eq!(ann.title.as_deref(), Some("Greet"));

    let out = tool
        .output_schema
        .as_ref()
        .expect("output_schema populated");
    let json = serde_json::to_value(out).unwrap();
    // schemars 1.x emits the schema dialect at the root.
    let dialect = json
        .get("$schema")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        dialect.contains("2020-12") || dialect.contains("draft-07"),
        "outputSchema should advertise a JSON Schema dialect, got: {dialect}"
    );
    // Sanity-check that the schema actually describes `GreetingOut` rather
    // than collapsing to the default empty object — the `greeting: String`
    // property must be present.
    let greeting_prop = json.get("properties").and_then(|p| p.get("greeting"));
    assert!(
        greeting_prop.is_some(),
        "outputSchema must reflect the `greeting` field of GreetingOut, got: {json}"
    );
    assert_eq!(
        greeting_prop
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str()),
        Some("string"),
        "outputSchema's `greeting` field should be typed as string"
    );

    let resources = server.list_resources();
    let res = resources
        .iter()
        .find(|r| r.name == "config")
        .expect("config resource");
    assert_eq!(res.title.as_deref(), Some("App config"));
    assert_eq!(res.icons.as_ref().expect("res icons").len(), 1);
    assert_eq!(res.mime_type.as_deref(), Some("application/json"));

    let prompts = server.list_prompts();
    let pr = prompts
        .iter()
        .find(|p| p.name == "summarize")
        .expect("summarize prompt");
    assert_eq!(pr.title.as_deref(), Some("Summarize"));
    assert_eq!(pr.icons.as_ref().expect("prompt icons").len(), 1);
}

// SEP-1613: every macro-generated tool schema must declare the JSON Schema
// 2020-12 dialect via `$schema`. Clients use this to pick the right validator.
#[test]
fn macro_schemas_declare_2020_12_dialect() {
    let server = AnnotatedServer;
    let tools = server.list_tools();
    let tool = tools
        .iter()
        .find(|t| t.name == "greet")
        .expect("greet tool");

    let json = serde_json::to_value(&tool.input_schema).expect("inputSchema serializable");
    let dialect = json
        .get("$schema")
        .and_then(|v| v.as_str())
        .expect("inputSchema must declare $schema");
    assert_eq!(
        dialect, "https://json-schema.org/draft/2020-12/schema",
        "inputSchema $schema must be the 2020-12 dialect URI"
    );
}

// SEP-1303 (MCP 2025-11-25): tool input validation failures must surface as
// `CallToolResult { isError: true, content: [...] }` so the model can
// self-correct, NOT as JSON-RPC -32602 protocol errors.
#[tokio::test]
async fn invalid_args_return_tool_execution_error() {
    let server = AnnotatedServer;
    let ctx = RequestContext::stdio();

    // Missing required `name` parameter — historically this returned
    // Err(McpError::invalid_params(...)). After SEP-1303 it must come back as
    // an Ok result with isError = true.
    let result = server
        .call_tool("greet", serde_json::json!({}), &ctx)
        .await
        .expect("validation failure must surface as Ok(ToolResult), not Err");
    assert!(
        result.is_error(),
        "result must carry isError = true to signal a tool execution error"
    );
    let text = result.first_text().unwrap_or_default();
    assert!(
        text.to_lowercase().contains("name"),
        "execution error message should reference the offending parameter, got: {text}"
    );

    // Wrong-type argument also routes through SEP-1303.
    let result = server
        .call_tool("greet", serde_json::json!({ "name": 42 }), &ctx)
        .await
        .expect("type-coercion failure must surface as Ok(ToolResult), not Err");
    assert!(result.is_error());
}

#[tokio::test]
async fn dangerous_uri_schemes_are_still_rejected() {
    let server = CustomSchemeServer;
    let ctx = RequestContext::stdio();

    let err = server
        .read_resource("javascript:alert(1)", &ctx)
        .await
        .expect_err("javascript: scheme must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("javascript"),
        "error should mention the rejected scheme, got: {msg}"
    );

    let err = server
        .read_resource("vbscript:msgbox(1)", &ctx)
        .await
        .expect_err("vbscript: scheme must be rejected");
    assert!(err.to_string().contains("vbscript"));
}
