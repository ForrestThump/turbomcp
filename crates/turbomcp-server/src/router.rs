//! JSON-RPC request routing for McpHandler.
//!
//! This module provides the native server's request routing with additional
//! configuration validation beyond the core router.
//!
//! # Architecture
//!
//! The native router layers on top of `turbomcp_core::router`:
//! - **Core router**: Basic MCP method dispatch (shared with WASM)
//! - **Native router**: Protocol negotiation, capability validation
//!
//! # MCP Protocol Compliance
//!
//! This router implements the MCP 2025-11-25 specification:
//! - Initialize request validates `clientInfo` and `protocolVersion`
//! - Notifications (requests without `id`) do not receive responses
//! - Capability structure follows the spec format
//! - Error codes follow JSON-RPC 2.0 standard

use std::collections::HashSet;

use super::config::{ClientCapabilities, SearchToolsConfig, ServerConfig};
use turbomcp_core::context::RequestContext;
use turbomcp_core::error::McpError;
use turbomcp_core::handler::McpHandler;
use turbomcp_protocol::versioning::adapter::{VersionAdapter, adapter_for_version};

// Re-export canonical JSON-RPC types from turbomcp-core
pub use turbomcp_core::jsonrpc::{JsonRpcIncoming, JsonRpcOutgoing};
// Re-export core router utilities
pub use turbomcp_core::router::{parse_request, parse_request_from_value, serialize_response};

/// Route a JSON-RPC request to the appropriate handler method.
///
/// This is the simple routing function that uses default configuration.
/// For more control, use `route_request_with_config`.
pub async fn route_request<H: McpHandler>(
    handler: &H,
    request: JsonRpcIncoming,
    ctx: &RequestContext,
) -> JsonRpcOutgoing {
    route_request_with_config(handler, request, ctx, None).await
}

/// Route a JSON-RPC request with custom server configuration.
///
/// This function provides full control over protocol negotiation,
/// capability validation, and other server behavior.
///
/// # Additional Validation (vs core router)
///
/// When a `ServerConfig` is provided, this function adds:
/// - Protocol version negotiation
/// - Required client capability validation
pub async fn route_request_with_config<H: McpHandler>(
    handler: &H,
    request: JsonRpcIncoming,
    ctx: &RequestContext,
    config: Option<&ServerConfig>,
) -> JsonRpcOutgoing {
    if request.is_notification() {
        return JsonRpcOutgoing::notification_ack();
    }

    let id = request.id.clone();

    // Validate message size against configured limit
    if let Some(config) = config
        && let Some(ref params) = request.params
    {
        let estimated_size = params.to_string().len();
        if estimated_size > config.max_message_size {
            return JsonRpcOutgoing::error(
                id,
                McpError::invalid_request(format!(
                    "Message size {} exceeds maximum allowed size of {} bytes",
                    estimated_size, config.max_message_size
                )),
            );
        }
    }

    // For initialize requests, apply native-specific validation
    if request.method == "initialize" {
        let params_owned;
        let params = match request.params.as_ref() {
            Some(p) => p,
            None => {
                params_owned = serde_json::Value::default();
                &params_owned
            }
        };

        // Validate clientInfo is present (MCP spec requirement)
        let Some(client_info) = params.get("clientInfo") else {
            return JsonRpcOutgoing::error(
                id,
                McpError::invalid_params("Missing required field: clientInfo"),
            );
        };

        // Validate clientInfo has required fields
        let client_name = client_info.get("name").and_then(|v| v.as_str());
        let client_version = client_info.get("version").and_then(|v| v.as_str());
        let Some(name) = client_name else {
            return JsonRpcOutgoing::error(
                id,
                McpError::invalid_params("clientInfo must contain 'name' and 'version' fields"),
            );
        };
        let Some(version) = client_version else {
            return JsonRpcOutgoing::error(
                id,
                McpError::invalid_params("clientInfo must contain 'name' and 'version' fields"),
            );
        };
        // Bound length and reject empty / control-char values so they don't
        // become a log-injection / telemetry-noise vector.
        const CLIENT_INFO_MAX_LEN: usize = 128;
        let is_bad = |s: &str| {
            let trimmed = s.trim();
            trimmed.is_empty()
                || trimmed.len() > CLIENT_INFO_MAX_LEN
                || trimmed.chars().any(|c| c.is_control())
        };
        if is_bad(name) || is_bad(version) {
            return JsonRpcOutgoing::error(
                id,
                McpError::invalid_params(
                    "clientInfo.name / clientInfo.version must be non-empty, \
                     <=128 chars, and contain no control characters",
                ),
            );
        }

        // Extract client's requested protocol version
        let protocol_version = params.get("protocolVersion").and_then(|v| v.as_str());

        // Get protocol config (use default if none provided)
        let protocol_config = config.map(|c| &c.protocol).cloned().unwrap_or_default();

        // Negotiate protocol version
        let negotiated_version = match protocol_config.negotiate(protocol_version) {
            Some(version) => {
                // Log if server fell back to a different version
                if let Some(client_ver) = protocol_version
                    && client_ver != version
                {
                    tracing::warn!(
                        client_version = client_ver,
                        negotiated_version = %version,
                        supported = ?protocol_config.supported_versions,
                        "Protocol version fallback: client requested unsupported version"
                    );
                }
                version
            }
            None => {
                return JsonRpcOutgoing::error(
                    id,
                    McpError::invalid_request(format!(
                        "Unsupported protocol version: {}. Supported versions: {:?}",
                        protocol_version.unwrap_or("none"),
                        protocol_config.supported_versions
                    )),
                );
            }
        };

        // Parse and validate client capabilities if required
        if let Some(cfg) = config {
            let client_caps = ClientCapabilities::from_params(params);
            let validation = cfg.required_capabilities.validate(&client_caps);

            if let Some(missing) = validation.missing() {
                return JsonRpcOutgoing::error(
                    id,
                    McpError::invalid_request(format!(
                        "Missing required client capabilities: {}",
                        missing.join(", ")
                    )),
                );
            }
        }

        // Use core router with negotiated version
        let version_str = negotiated_version.as_str();
        let core_config = turbomcp_core::router::RouteConfig {
            protocol_version: Some(version_str),
        };
        let response =
            turbomcp_core::router::route_request(handler, request, ctx, &core_config).await;

        // Apply version adapter to the initialize response
        let adapter = adapter_for_version(&negotiated_version);
        return apply_adapter_to_response(adapter, "initialize", response);
    }

    // For all other methods, apply tool filtering then delegate to core router.
    let disabled = config.map(|c| &c.disabled_tools);
    let hidden = config.map(|c| &c.hidden_tools);
    let search_cfg =
        config.and_then(|c| if c.search_tools.enabled { Some(&c.search_tools) } else { None });

    if request.method == "tools/call" {
        // Intercept the built-in search tool before it reaches the handler.
        if let Some(cfg) = search_cfg {
            let call_name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if call_name == cfg.tool_name {
                return handle_search_tools(handler, &request, config).await;
            }
        }

        // Pre-check: reject calls to disabled tools before hitting the core router.
        if let Some(disabled) = disabled.filter(|d| !d.is_empty()) {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if disabled.contains(name) {
                return JsonRpcOutgoing::error(id, McpError::tool_not_found(name));
            }
        }
    }

    let method = request.method.clone();
    let core_config = turbomcp_core::router::RouteConfig::default();
    let response = turbomcp_core::router::route_request(handler, request, ctx, &core_config).await;

    // Post-filter tools/list: strip disabled, strip hidden, inject search_tools.
    if method == "tools/list" {
        let response = if let Some(disabled) = disabled.filter(|d| !d.is_empty()) {
            filter_disabled_tools(response, disabled)
        } else {
            response
        };
        let response = if let Some(hidden) = hidden.filter(|h| !h.is_empty()) {
            filter_hidden_tools(response, hidden)
        } else {
            response
        };
        return if let Some(cfg) = search_cfg {
            inject_search_tool(response, cfg)
        } else {
            response
        };
    }

    response
}

/// Route a JSON-RPC request with version-aware adapter filtering.
///
/// This is the recommended entry point for post-initialize requests when the
/// session has a negotiated protocol version. It:
/// 1. Validates the method is available in the negotiated version
/// 2. Applies tool-disable filtering from `config` (if provided)
/// 3. Delegates to the core router
/// 4. Applies the version adapter to filter the response
///
/// Transport layers should store the negotiated [`turbomcp_protocol::types::ProtocolVersion`] from
/// the initialize handshake and pass it here for all subsequent requests.
pub async fn route_request_versioned<H: McpHandler>(
    handler: &H,
    request: JsonRpcIncoming,
    ctx: &RequestContext,
    negotiated_version: &turbomcp_types::ProtocolVersion,
    config: Option<&ServerConfig>,
) -> JsonRpcOutgoing {
    if request.is_notification() {
        return JsonRpcOutgoing::notification_ack();
    }

    let adapter = adapter_for_version(negotiated_version);
    let method = request.method.clone();

    // Validate that the method exists in the negotiated version
    if let Err(reason) = adapter.validate_method(&method) {
        return JsonRpcOutgoing::error(request.id.clone(), McpError::method_not_found(reason));
    }

    let disabled = config.map(|c| &c.disabled_tools);
    let hidden = config.map(|c| &c.hidden_tools);
    let search_cfg =
        config.and_then(|c| if c.search_tools.enabled { Some(&c.search_tools) } else { None });

    if method == "tools/call" {
        // Intercept the built-in search tool before it reaches the handler.
        if let Some(cfg) = search_cfg {
            let call_name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if call_name == cfg.tool_name {
                return handle_search_tools(handler, &request, config).await;
            }
        }

        // Pre-check: reject calls to disabled tools before hitting the core router.
        if let Some(disabled) = disabled.filter(|d| !d.is_empty()) {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if disabled.contains(name) {
                return JsonRpcOutgoing::error(
                    request.id.clone(),
                    McpError::tool_not_found(name),
                );
            }
        }
    }

    // Route through core
    let core_config = turbomcp_core::router::RouteConfig::default();
    let response = turbomcp_core::router::route_request(handler, request, ctx, &core_config).await;

    // Post-filter tools/list: strip disabled, strip hidden, inject search_tools.
    let response = if method == "tools/list" {
        let response = if let Some(disabled) = disabled.filter(|d| !d.is_empty()) {
            filter_disabled_tools(response, disabled)
        } else {
            response
        };
        let response = if let Some(hidden) = hidden.filter(|h| !h.is_empty()) {
            filter_hidden_tools(response, hidden)
        } else {
            response
        };
        if let Some(cfg) = search_cfg {
            inject_search_tool(response, cfg)
        } else {
            response
        }
    } else {
        response
    };

    // Apply version adapter to filter the response
    apply_adapter_to_response(adapter, &method, response)
}

/// Remove disabled tools from a `tools/list` response.
fn filter_disabled_tools(
    mut response: JsonRpcOutgoing,
    disabled: &HashSet<String>,
) -> JsonRpcOutgoing {
    if let Some(result) = response.result.as_mut() {
        if let Some(tools) = result.get_mut("tools").and_then(|v| v.as_array_mut()) {
            tools.retain(|t| {
                t.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| !disabled.contains(n))
                    .unwrap_or(true)
            });
        }
    }
    response
}

/// Remove hidden tools from a `tools/list` response.
///
/// Hidden tools are suppressed from the listing but remain callable and appear
/// in `search_tools` results.
fn filter_hidden_tools(
    mut response: JsonRpcOutgoing,
    hidden: &HashSet<String>,
) -> JsonRpcOutgoing {
    if let Some(result) = response.result.as_mut() {
        if let Some(tools) = result.get_mut("tools").and_then(|v| v.as_array_mut()) {
            tools.retain(|t| {
                t.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| !hidden.contains(n))
                    .unwrap_or(true)
            });
        }
    }
    response
}

/// Append the built-in `search_tools` entry to a `tools/list` response.
fn inject_search_tool(mut response: JsonRpcOutgoing, cfg: &SearchToolsConfig) -> JsonRpcOutgoing {
    if let Some(result) = response.result.as_mut() {
        if let Some(tools) = result.get_mut("tools").and_then(|v| v.as_array_mut()) {
            tools.push(search_tool_definition(&cfg.tool_name));
        }
    }
    response
}

/// Build the JSON schema definition for the built-in search tool.
fn search_tool_definition(tool_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": tool_name,
        "description": "Search for available tools by name or description. \
            Returns tools matching the query, including tools that are not \
            shown in the main tool listing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search terms matched case-insensitively \
                        against tool names and descriptions (partial match)."
                }
            },
            "required": ["query"]
        }
    })
}

/// Handle a `tools/call` request for the built-in search tool.
///
/// Searches `handler.list_tools()`, excludes disabled tools, applies the query,
/// and returns matching tool definitions as a text content block.
async fn handle_search_tools<H: McpHandler>(
    handler: &H,
    request: &JsonRpcIncoming,
    config: Option<&ServerConfig>,
) -> JsonRpcOutgoing {
    let query = request
        .params
        .as_ref()
        .and_then(|p| p.get("arguments"))
        .and_then(|a| a.get("query"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();

    let disabled = config.map(|c| &c.disabled_tools);

    let matching: Vec<serde_json::Value> = handler
        .list_tools()
        .into_iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .filter(|t| {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            if let Some(disabled) = disabled.filter(|d| !d.is_empty()) {
                if disabled.contains(name) {
                    return false;
                }
            }
            if query.is_empty() {
                return true;
            }
            let desc = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            name.to_lowercase().contains(&query) || desc.to_lowercase().contains(&query)
        })
        .collect();

    let text = if matching.is_empty() {
        format!("No tools found matching '{query}'.")
    } else {
        match serde_json::to_string_pretty(&matching) {
            Ok(json) => format!("Found {} tool(s):\n{json}", matching.len()),
            Err(_) => "Error serializing tool results.".to_string(),
        }
    };

    JsonRpcOutgoing::success(
        request.id.clone(),
        serde_json::json!({
            "content": [{"type": "text", "text": text}]
        }),
    )
}

/// Apply a version adapter to a JSON-RPC response.
///
/// This filters the result value through the adapter's `filter_result` method,
/// stripping fields that don't exist in the target spec version.
///
/// Transport layers should call this on outgoing responses when the session
/// has a negotiated version different from the latest.
///
/// # Example
///
/// ```rust,ignore
/// use turbomcp_server::router::apply_adapter_to_response;
/// use turbomcp_protocol::versioning::adapter::adapter_for_version;
///
/// let adapter = adapter_for_version(&negotiated_version);
/// let filtered = apply_adapter_to_response(adapter, "tools/list", response);
/// ```
pub fn apply_adapter_to_response(
    adapter: &dyn VersionAdapter,
    method: &str,
    mut response: JsonRpcOutgoing,
) -> JsonRpcOutgoing {
    // Only filter successful responses (errors pass through unchanged)
    if let Some(result) = response.result.take() {
        response.result = Some(adapter.filter_result(method, result));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use turbomcp_core::error::McpResult;
    use turbomcp_types::{
        Prompt, PromptResult, Resource, ResourceResult, ServerInfo, Tool, ToolResult,
    };

    #[derive(Clone)]
    struct TestHandler;

    impl McpHandler for TestHandler {
        fn server_info(&self) -> ServerInfo {
            ServerInfo::new("test", "1.0.0")
        }

        fn list_tools(&self) -> Vec<Tool> {
            vec![Tool::new("test_tool", "A test tool")]
        }

        fn list_resources(&self) -> Vec<Resource> {
            vec![]
        }

        fn list_prompts(&self) -> Vec<Prompt> {
            vec![]
        }

        fn call_tool(
            &self,
            name: &str,
            _args: Value,
            _ctx: &RequestContext,
        ) -> impl std::future::Future<Output = McpResult<ToolResult>> + Send {
            let name = name.to_string();
            async move {
                if name == "test_tool" {
                    Ok(ToolResult::text("Tool executed"))
                } else {
                    Err(McpError::tool_not_found(&name))
                }
            }
        }

        fn read_resource(
            &self,
            uri: &str,
            _ctx: &RequestContext,
        ) -> impl std::future::Future<Output = McpResult<ResourceResult>> + Send {
            let uri = uri.to_string();
            async move { Err(McpError::resource_not_found(&uri)) }
        }

        fn get_prompt(
            &self,
            name: &str,
            _args: Option<Value>,
            _ctx: &RequestContext,
        ) -> impl std::future::Future<Output = McpResult<PromptResult>> + Send {
            let name = name.to_string();
            async move { Err(McpError::prompt_not_found(&name)) }
        }
    }

    #[test]
    fn test_parse_request() {
        let input = r#"{"jsonrpc": "2.0", "id": 1, "method": "ping"}"#;
        let request = parse_request(input).unwrap();
        assert_eq!(request.method, "ping");
        assert_eq!(request.id, Some(serde_json::json!(1)));
    }

    #[test]
    fn test_serialize_response() {
        let response = JsonRpcOutgoing::success(Some(serde_json::json!(1)), serde_json::json!({}));
        let serialized = serialize_response(&response).unwrap();
        assert!(serialized.contains("\"jsonrpc\":\"2.0\""));
        assert!(serialized.contains("\"id\":1"));
    }

    #[tokio::test]
    async fn test_route_initialize() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "initialize".to_string(),
            // MCP spec requires clientInfo with name and version
            params: Some(serde_json::json!({
                "protocolVersion": "2025-11-25",
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                },
                "capabilities": {}
            })),
        };

        let response = route_request(&handler, request, &ctx).await;
        assert!(response.result.is_some());
        assert!(response.error.is_none());

        let result = response.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "test");
        // Verify capabilities structure per MCP spec
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
    }

    #[tokio::test]
    async fn test_route_initialize_missing_client_info() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "2025-11-25"
            })),
        };

        let response = route_request(&handler, request, &ctx).await;
        assert!(response.error.is_some());
        let error = response.error.unwrap();
        assert_eq!(error.code, -32602); // INVALID_PARAMS
        assert!(error.message.contains("clientInfo"));
    }

    #[tokio::test]
    async fn test_route_initialized_notification() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        // Notification has no id
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "notifications/initialized".to_string(),
            params: None,
        };

        let response = route_request(&handler, request, &ctx).await;
        // Notification responses should not be sent
        assert!(!response.should_send());
    }

    #[tokio::test]
    async fn test_route_request_method_without_id_is_not_sent() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request(&handler, request, &ctx).await;
        assert!(!response.should_send());
    }

    #[tokio::test]
    async fn test_route_tools_list() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request(&handler, request, &ctx).await;
        assert!(response.result.is_some());

        let result = response.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "test_tool");
    }

    #[tokio::test]
    async fn test_route_tools_call() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "test_tool",
                "arguments": {}
            })),
        };

        let response = route_request(&handler, request, &ctx).await;
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn test_disabled_tool_hidden_from_list() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .disable_tool("test_tool")
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.result.is_some());
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(tools.is_empty(), "disabled tool should be absent from list");
    }

    #[tokio::test]
    async fn test_disabled_tool_call_rejected() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .disable_tool("test_tool")
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "test_tool",
                "arguments": {}
            })),
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.result.is_none());
        assert!(response.error.is_some());
    }

    #[tokio::test]
    async fn test_non_disabled_tool_still_accessible() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .disable_tool("other_tool")
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 1, "non-disabled tool should remain visible");
        assert_eq!(tools[0]["name"], "test_tool");
    }

    #[tokio::test]
    async fn test_hidden_tool_absent_from_list() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .hide_tool("test_tool")
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(tools.is_empty(), "hidden tool should be absent from list");
    }

    #[tokio::test]
    async fn test_hidden_tool_still_callable() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .hide_tool("test_tool")
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({"name": "test_tool", "arguments": {}})),
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.error.is_none(), "hidden tool must remain callable");
        assert!(response.result.is_some());
    }

    #[tokio::test]
    async fn test_search_tools_not_injected_by_default() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder().build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(
            tools.iter().all(|t| t["name"] != "search_tools"),
            "search_tools must not appear when disabled"
        );
    }

    #[tokio::test]
    async fn test_search_tools_injected_when_enabled() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .enable_search_tools()
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(
            tools.iter().any(|t| t["name"] == "search_tools"),
            "search_tools must appear when enabled"
        );
    }

    #[tokio::test]
    async fn test_search_tools_returns_matching_tools() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .enable_search_tools()
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "search_tools",
                "arguments": {"query": "test"}
            })),
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("test_tool"), "search should find test_tool");
    }

    #[tokio::test]
    async fn test_search_tools_finds_hidden_tools() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .hide_tool("test_tool")
            .enable_search_tools()
            .build();

        // Confirm the tool is hidden from list
        let list_req = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        let list_resp =
            route_request_with_config(&handler, list_req, &ctx, Some(&config)).await;
        let tools = list_resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(
            tools.iter().all(|t| t["name"] != "test_tool"),
            "hidden tool must not appear in list"
        );

        // But search should find it
        let search_req = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(2)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "search_tools",
                "arguments": {"query": "test"}
            })),
        };
        let search_resp =
            route_request_with_config(&handler, search_req, &ctx, Some(&config)).await;
        assert!(search_resp.error.is_none());
        let text = search_resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("test_tool"), "search must find hidden tools");
    }

    #[tokio::test]
    async fn test_search_tools_excludes_disabled_tools() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .disable_tool("test_tool")
            .enable_search_tools()
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "search_tools",
                "arguments": {"query": "test"}
            })),
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.error.is_none());
        let text = response.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            !text.contains("test_tool"),
            "search must not surface disabled tools"
        );
    }

    #[tokio::test]
    async fn test_search_tools_empty_query_returns_all() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .enable_search_tools()
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "search_tools",
                "arguments": {"query": ""}
            })),
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.error.is_none());
        let text = response.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("test_tool"), "empty query should return all tools");
    }

    #[tokio::test]
    async fn test_search_tools_no_match() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .enable_search_tools()
            .build();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "search_tools",
                "arguments": {"query": "zzz_no_match_at_all"}
            })),
        };

        let response = route_request_with_config(&handler, request, &ctx, Some(&config)).await;
        assert!(response.error.is_none());
        let text = response.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            text.contains("No tools found"),
            "unmatched query should produce empty-result message"
        );
    }

    #[tokio::test]
    async fn test_search_tools_custom_name_in_list_and_callable() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .enable_search_tools_named("find_tool")
            .build();

        // Custom name appears in the tool list; default name does not.
        let list_req = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        let list_resp =
            route_request_with_config(&handler, list_req, &ctx, Some(&config)).await;
        let tools = list_resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(
            tools.iter().any(|t| t["name"] == "find_tool"),
            "custom name must appear in tools/list"
        );
        assert!(
            tools.iter().all(|t| t["name"] != "search_tools"),
            "default name must not appear when overridden"
        );

        // Custom name is callable.
        let call_req = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(2)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "find_tool",
                "arguments": {"query": "test"}
            })),
        };
        let call_resp =
            route_request_with_config(&handler, call_req, &ctx, Some(&config)).await;
        assert!(call_resp.error.is_none(), "custom-named search tool must be callable");
    }

    // ── route_request_versioned path (production hot path) ───────────────

    #[tokio::test]
    async fn test_route_versioned_hidden_tool_absent_from_list() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .hide_tool("test_tool")
            .build();
        let version = turbomcp_types::ProtocolVersion::LATEST.clone();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response =
            route_request_versioned(&handler, request, &ctx, &version, Some(&config)).await;
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(
            tools.is_empty(),
            "hidden tool must be absent from list via versioned path"
        );
    }

    #[tokio::test]
    async fn test_route_versioned_search_tools_injected() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let config = super::super::config::ServerConfig::builder()
            .enable_search_tools()
            .build();
        let version = turbomcp_types::ProtocolVersion::LATEST.clone();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };

        let response =
            route_request_versioned(&handler, request, &ctx, &version, Some(&config)).await;
        let tools = response.result.unwrap()["tools"].as_array().unwrap().clone();
        assert!(
            tools.iter().any(|t| t["name"] == "search_tools"),
            "search_tools must be injected via versioned path"
        );
    }

    #[tokio::test]
    async fn test_route_unknown_method() {
        let handler = TestHandler;
        let ctx = RequestContext::stdio();
        let request = JsonRpcIncoming {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "unknown/method".to_string(),
            params: None,
        };

        let response = route_request(&handler, request, &ctx).await;
        assert!(response.result.is_none());
        assert!(response.error.is_some());

        let error = response.error.unwrap();
        assert_eq!(error.code, -32601); // METHOD_NOT_FOUND
    }
}
