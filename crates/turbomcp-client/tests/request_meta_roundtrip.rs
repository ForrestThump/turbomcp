//! Round-trip test for request `_meta`: a client `call_tool_with_meta` must serialize `_meta`
//! onto the wire, the server router must surface it into the handler's `RequestContext`, and the
//! handler must be able to read it back. We drive a real in-process server over the channel
//! transport and have its tool echo the `_meta` it received, then assert the client got it back.

use core::future::Future;
use serde_json::Value;
use turbomcp_client::Client;
use turbomcp_core::context::{REQUEST_META_KEY, RequestContext};
use turbomcp_core::error::{McpError, McpResult};
use turbomcp_core::handler::McpHandler;
use turbomcp_core::marker::MaybeSend;
use turbomcp_types::{
    Prompt, PromptResult, Resource, ResourceResult, ServerInfo, Tool, ToolResult,
};

/// A minimal handler whose `echo_meta` tool returns the request `_meta` the router surfaced into
/// its context (or `<none>`), so the test can assert the value made the full round-trip.
#[derive(Clone)]
struct EchoMetaHandler;

impl McpHandler for EchoMetaHandler {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new("echo-meta", "1.0.0")
    }

    fn list_tools(&self) -> Vec<Tool> {
        vec![Tool::new("echo_meta", "Echo the request _meta")]
    }

    fn list_resources(&self) -> Vec<Resource> {
        Vec::new()
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        Vec::new()
    }

    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        _args: Value,
        ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
        let echoed = ctx
            .get_metadata(REQUEST_META_KEY)
            .map(|m| m.to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let name = name.to_string();
        async move {
            match name.as_str() {
                "echo_meta" => Ok(ToolResult::text(echoed)),
                _ => Err(McpError::tool_not_found(&name)),
            }
        }
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        _ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
        let uri = uri.to_string();
        async move { Err(McpError::resource_not_found(&uri)) }
    }

    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
        _args: Option<Value>,
        _ctx: &'a RequestContext,
    ) -> impl Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
        let name = name.to_string();
        async move { Err(McpError::prompt_not_found(&name)) }
    }
}

#[tokio::test]
async fn call_tool_with_meta_round_trips_to_the_handler() {
    let handler = EchoMetaHandler;
    let (transport, server) = turbomcp_server::transport::channel::run_in_process(&handler)
        .await
        .expect("start in-process server");

    let client = Client::new(transport);
    client.initialize().await.expect("initialize");

    let meta = serde_json::json!({
        "_liberado_provenance": { "source": "tasks-mcp", "correlation_id": "corr-1" }
    });
    let result = client
        .call_tool_with_meta("echo_meta", None, Some(meta.clone()))
        .await
        .expect("call_tool_with_meta");

    // The server echoed back the `_meta` it received — proving client-send + router-surface.
    let echoed = serde_json::to_string(&result).expect("serialize result");
    assert!(
        echoed.contains("tasks-mcp") && echoed.contains("corr-1"),
        "server did not receive the request _meta; got: {echoed}"
    );

    // And without `_meta`, the handler sees nothing.
    let bare = client
        .call_tool_with_meta("echo_meta", None, None)
        .await
        .expect("call without meta");
    let bare = serde_json::to_string(&bare).expect("serialize");
    assert!(bare.contains("<none>"), "expected no meta; got: {bare}");

    server.abort();
}
