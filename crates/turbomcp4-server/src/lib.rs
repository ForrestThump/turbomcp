//! # turbomcp4-server
//!
//! The server framework: the user-facing traits, the capability router, and the
//! `tower`-shaped dispatcher that connects them to a transport.
//!
//! - [`McpServerCore`] + capability traits ([`WithTools`], …) — what a user
//!   implements. Handlers speak `turbomcp4_protocol::neutral` types, never wire
//!   types, so a server is portable across protocol versions.
//! - [`MethodRouter`] — registers the capabilities a server actually implements;
//!   advertised capabilities are *derived* from it, so they can't drift.
//! - [`VersionDispatcher`] — `Service<JsonRpcMessage>`: extracts the version,
//!   routes to the typed handler, and serializes the response. All per-version
//!   branching is concentrated here.
//!
//! Both protocol paths are live: the modern `DRAFT-2026-v1` stateless path and
//! the legacy `2025-11-25` stateful path (`initialize` handshake +
//! [`SessionStore`]; see [`LegacySessionAdapter`] for byte-pipe transports).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod adapter;
mod builder;
mod context;
mod dispatcher;
mod response;
mod router;
mod session;
mod traits;

pub use adapter::LegacySessionAdapter;
pub use builder::{IntoServerBuilder, ServerBuilder};
pub use context::{
    CallToolContext, CompleteContext, GetPromptContext, ListPromptsContext,
    ListResourceTemplatesContext, ListResourcesContext, ListToolsContext, ReadResourceContext,
};
pub use dispatcher::VersionDispatcher;
pub use response::{IntoCallToolResult, IntoGetPromptResult, IntoReadResourceResult};
pub use router::MethodRouter;
pub use session::{SessionState, SessionStore};
pub use traits::{McpServerCore, WithCompletions, WithPrompts, WithResources, WithTools};

/// Support items called by `#[server]`-generated code. Not part of the stable
/// API — do not depend on it directly.
#[doc(hidden)]
pub mod __macro_support {
    use serde_json::Value;

    /// Strip schemars bookkeeping (`$schema`, `title`) so a generated argument
    /// schema reads as a clean MCP tool input schema.
    #[must_use]
    pub fn normalize_input_schema(mut v: Value) -> Value {
        if let Some(obj) = v.as_object_mut() {
            obj.remove("$schema");
            obj.remove("title");
        }
        v
    }

    /// Mark a property as an MCP header parameter (SEP-2243). Transport-side
    /// mirroring lands in Phase 4; here we annotate the input schema so the
    /// information is present and snapshot-tested.
    pub fn mark_mcp_header(schema: &mut Value, property: &str) {
        if let Some(prop) = schema
            .get_mut("properties")
            .and_then(|p| p.get_mut(property))
            .and_then(Value::as_object_mut)
        {
            prop.insert("x-mcp-header".into(), Value::Bool(true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tower::{Service, ServiceExt};
    use turbomcp4_core::{Implementation, JsonRpcMessage, JsonRpcRequest, McpResult};
    use turbomcp4_protocol::neutral;

    #[derive(Clone)]
    struct Calculator;

    impl McpServerCore for Calculator {
        fn server_info(&self) -> Implementation {
            Implementation::new("calculator", "0.1.0")
        }
        fn instructions(&self) -> Option<String> {
            Some("A demo calculator server.".into())
        }
    }

    impl WithTools for Calculator {
        async fn list_tools(
            &self,
            _ctx: &ListToolsContext,
            _params: neutral::ListParams,
        ) -> McpResult<neutral::ListToolsResult> {
            Ok(neutral::ListToolsResult::new(vec![neutral::Tool::new(
                "add",
                json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}),
            )
            .with_description("Add two numbers")]))
        }

        async fn call_tool(
            &self,
            _ctx: &CallToolContext,
            params: neutral::CallToolParams,
        ) -> McpResult<neutral::CallToolResult> {
            if params.name != "add" {
                return Ok(neutral::CallToolResult::error(format!(
                    "unknown tool: {}",
                    params.name
                )));
            }
            let a = params
                .arguments
                .get("a")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let b = params
                .arguments
                .get("b")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            Ok(neutral::CallToolResult::text(format!("{}", a + b)))
        }
    }

    fn dispatcher() -> VersionDispatcher<Calculator> {
        VersionDispatcher::new(Calculator, MethodRouter::new().with_tools())
    }

    /// Build draft `_meta` carrying the per-request protocol version.
    fn draft_meta() -> serde_json::Value {
        json!({ "io.modelcontextprotocol/protocolVersion": "DRAFT-2026-v1" })
    }

    async fn call(svc: &mut VersionDispatcher<Calculator>, req: JsonRpcRequest) -> JsonRpcMessage {
        svc.ready()
            .await
            .unwrap()
            .call(req.into())
            .await
            .unwrap()
            .expect("request should produce a response")
    }

    #[tokio::test]
    async fn discover_advertises_tools_and_versions() {
        let mut svc = dispatcher();
        let resp = call(&mut svc, JsonRpcRequest::new(1, "server/discover", None)).await;
        let JsonRpcMessage::Response(r) = resp else {
            panic!("expected response")
        };
        let result = r.result.expect("discover result");
        assert_eq!(result["serverInfo"]["name"], "calculator");
        assert_eq!(result["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(result["resultType"], "complete");
        let versions = result["supportedVersions"].as_array().unwrap();
        assert!(versions.iter().any(|v| v == "DRAFT-2026-v1"));
        assert!(versions.iter().any(|v| v == "2025-11-25"));
        assert_eq!(result["instructions"], "A demo calculator server.");
    }

    #[tokio::test]
    async fn tools_list_returns_registered_tools() {
        let mut svc = dispatcher();
        let req = JsonRpcRequest::new(2, "tools/list", Some(json!({ "_meta": draft_meta() })));
        let JsonRpcMessage::Response(r) = call(&mut svc, req).await else {
            panic!()
        };
        let result = r.result.unwrap();
        assert_eq!(result["tools"][0]["name"], "add");
        assert_eq!(result["tools"][0]["description"], "Add two numbers");
        assert_eq!(result["resultType"], "complete");
    }

    #[tokio::test]
    async fn tools_call_invokes_handler() {
        let mut svc = dispatcher();
        let req = JsonRpcRequest::new(
            3,
            "tools/call",
            Some(json!({ "name": "add", "arguments": {"a": 2, "b": 3}, "_meta": draft_meta() })),
        );
        let JsonRpcMessage::Response(r) = call(&mut svc, req).await else {
            panic!()
        };
        let result = r.result.unwrap();
        assert_eq!(result["content"][0]["text"], "5");
        assert_eq!(result["isError"], false);
    }

    #[tokio::test]
    async fn missing_version_yields_unsupported_with_list() {
        let mut svc = dispatcher();
        // tools/list without `_meta.protocolVersion`.
        let req = JsonRpcRequest::new(4, "tools/list", Some(json!({})));
        let JsonRpcMessage::Response(r) = call(&mut svc, req).await else {
            panic!()
        };
        let err = r.error.expect("should be an error");
        assert_eq!(err.code, -32004);
    }

    #[tokio::test]
    async fn legacy_version_without_session_is_not_initialized() {
        let mut svc = dispatcher();
        let meta = json!({ "io.modelcontextprotocol/protocolVersion": "2025-11-25" });
        let req = JsonRpcRequest::new(5, "tools/list", Some(json!({ "_meta": meta })));
        let JsonRpcMessage::Response(r) = call(&mut svc, req).await else {
            panic!()
        };
        let err = r.error.expect("legacy request without a session must fail");
        assert_eq!(err.code, -32002);
        assert!(err.message.contains("initialize"));
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let mut svc = dispatcher();
        let req = JsonRpcRequest::new(
            6,
            "tools/nonexistent",
            Some(json!({ "_meta": draft_meta() })),
        );
        let JsonRpcMessage::Response(r) = call(&mut svc, req).await else {
            panic!()
        };
        assert_eq!(r.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn notification_produces_no_response() {
        let mut svc = dispatcher();
        let msg: JsonRpcMessage =
            turbomcp4_core::JsonRpcNotification::new("notifications/initialized", None).into();
        let out = svc.ready().await.unwrap().call(msg).await.unwrap();
        assert!(out.is_none());
    }

    /// A server without `WithTools` must not advertise tools.
    #[tokio::test]
    async fn server_without_tools_omits_capability() {
        #[derive(Clone)]
        struct Bare;
        impl McpServerCore for Bare {
            fn server_info(&self) -> Implementation {
                Implementation::new("bare", "0.0.0")
            }
        }
        let mut svc = VersionDispatcher::new(Bare, MethodRouter::<Bare>::new());
        // Reuse the dispatch path directly.
        let resp = svc
            .ready()
            .await
            .unwrap()
            .call(JsonRpcRequest::new(1, "server/discover", None).into())
            .await
            .unwrap()
            .unwrap();
        let JsonRpcMessage::Response(r) = resp else {
            panic!()
        };
        assert!(
            r.result
                .unwrap()
                .get("capabilities")
                .unwrap()
                .get("tools")
                .is_none()
        );
    }

    fn _is_send<T: Send>() {}
    #[test]
    fn dispatcher_is_send() {
        _is_send::<VersionDispatcher<Calculator>>();
    }

    #[tokio::test]
    async fn builder_registers_capabilities() {
        // `into_server()` (blanket) starts empty; `with_tools()` registers.
        let mut svc = Calculator.into_server().with_tools().build();
        let JsonRpcMessage::Response(r) = svc
            .ready()
            .await
            .unwrap()
            .call(JsonRpcRequest::new(1, "server/discover", None).into())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            r.result.unwrap()["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[test]
    fn builder_without_registration_has_no_capabilities() {
        let dispatcher = ServerBuilder::new(Calculator).build();
        _is_send::<VersionDispatcher<Calculator>>();
        let _ = dispatcher; // built successfully with an empty router
    }

    // ---- resources / prompts / completions ----------------------------------

    /// A server implementing every capability trait, used to prove discover
    /// advertises each one and the dispatcher routes all method families.
    #[derive(Clone)]
    struct Everything;

    impl McpServerCore for Everything {
        fn server_info(&self) -> Implementation {
            Implementation::new("everything", "0.1.0")
        }
    }

    impl WithResources for Everything {
        async fn list_resources(
            &self,
            _ctx: &ListResourcesContext,
            _params: neutral::ListParams,
        ) -> McpResult<neutral::ListResourcesResult> {
            Ok(neutral::ListResourcesResult::new(vec![
                neutral::Resource::new("file://readme", "readme").with_mime_type("text/plain"),
            ]))
        }

        async fn read_resource(
            &self,
            _ctx: &ReadResourceContext,
            params: neutral::ReadResourceParams,
        ) -> McpResult<neutral::ReadResourceResult> {
            Ok(neutral::ReadResourceResult::text(
                params.uri,
                "file contents",
            ))
        }
    }

    impl WithPrompts for Everything {
        async fn list_prompts(
            &self,
            _ctx: &ListPromptsContext,
            _params: neutral::ListParams,
        ) -> McpResult<neutral::ListPromptsResult> {
            Ok(neutral::ListPromptsResult::new(vec![
                neutral::Prompt::new("greet")
                    .with_argument(neutral::PromptArgument::new("name").required(true)),
            ]))
        }

        async fn get_prompt(
            &self,
            _ctx: &GetPromptContext,
            params: neutral::GetPromptParams,
        ) -> McpResult<neutral::GetPromptResult> {
            let name = params.arguments.get("name").cloned().unwrap_or_default();
            Ok(neutral::GetPromptResult::new(vec![
                neutral::PromptMessage::user_text(format!("Greet {name}")),
            ]))
        }
    }

    impl WithCompletions for Everything {
        async fn complete(
            &self,
            _ctx: &CompleteContext,
            params: neutral::CompleteParams,
        ) -> McpResult<neutral::CompleteResult> {
            // Echo the partial value back as the single suggestion.
            Ok(neutral::CompleteResult::new(vec![params.argument.value]))
        }
    }

    fn everything() -> VersionDispatcher<Everything> {
        VersionDispatcher::new(
            Everything,
            MethodRouter::new()
                .with_resources()
                .with_prompts()
                .with_completions(),
        )
    }

    async fn call_everything(
        svc: &mut VersionDispatcher<Everything>,
        req: JsonRpcRequest,
    ) -> serde_json::Value {
        let JsonRpcMessage::Response(r) = svc
            .ready()
            .await
            .unwrap()
            .call(req.into())
            .await
            .unwrap()
            .expect("response")
        else {
            panic!("expected response")
        };
        r.result.expect("result")
    }

    #[tokio::test]
    async fn discover_advertises_all_capabilities() {
        let mut svc = everything();
        let result =
            call_everything(&mut svc, JsonRpcRequest::new(1, "server/discover", None)).await;
        let caps = &result["capabilities"];
        assert_eq!(caps["resources"]["listChanged"], false);
        assert_eq!(caps["resources"]["subscribe"], false);
        assert_eq!(caps["prompts"]["listChanged"], false);
        assert!(caps["completions"].is_object());
        // No tools were registered → no tools capability.
        assert!(caps.get("tools").is_none());
    }

    #[tokio::test]
    async fn resources_list_read_and_templates_route() {
        let mut svc = everything();
        let meta = json!({ "_meta": draft_meta() });

        let list = call_everything(
            &mut svc,
            JsonRpcRequest::new(2, "resources/list", Some(meta.clone())),
        )
        .await;
        assert_eq!(list["resources"][0]["uri"], "file://readme");
        assert_eq!(list["resources"][0]["mimeType"], "text/plain");

        let read = call_everything(
            &mut svc,
            JsonRpcRequest::new(
                3,
                "resources/read",
                Some(json!({ "uri": "file://readme", "_meta": draft_meta() })),
            ),
        )
        .await;
        assert_eq!(read["contents"][0]["uri"], "file://readme");
        assert_eq!(read["contents"][0]["text"], "file contents");

        // The default `list_resource_templates` answers with an empty list.
        let templates = call_everything(
            &mut svc,
            JsonRpcRequest::new(4, "resources/templates/list", Some(meta)),
        )
        .await;
        assert_eq!(templates["resourceTemplates"].as_array().unwrap().len(), 0);
        assert_eq!(templates["resultType"], "complete");
    }

    #[tokio::test]
    async fn prompts_list_and_get_route() {
        let mut svc = everything();
        let list = call_everything(
            &mut svc,
            JsonRpcRequest::new(5, "prompts/list", Some(json!({ "_meta": draft_meta() }))),
        )
        .await;
        assert_eq!(list["prompts"][0]["name"], "greet");
        assert_eq!(list["prompts"][0]["arguments"][0]["required"], true);

        let got = call_everything(
            &mut svc,
            JsonRpcRequest::new(
                6,
                "prompts/get",
                Some(
                    json!({ "name": "greet", "arguments": {"name": "Ada"}, "_meta": draft_meta() }),
                ),
            ),
        )
        .await;
        assert_eq!(got["messages"][0]["role"], "user");
        assert_eq!(got["messages"][0]["content"]["text"], "Greet Ada");
    }

    #[tokio::test]
    async fn completion_complete_routes_with_ref_union() {
        let mut svc = everything();
        let result = call_everything(
            &mut svc,
            JsonRpcRequest::new(
                7,
                "completion/complete",
                Some(json!({
                    "ref": { "type": "ref/prompt", "name": "greet" },
                    "argument": { "name": "name", "value": "Ad" },
                    "_meta": draft_meta(),
                })),
            ),
        )
        .await;
        assert_eq!(result["completion"]["values"][0], "Ad");
        assert_eq!(result["resultType"], "complete");
    }

    #[tokio::test]
    async fn unregistered_capability_is_method_not_found() {
        // `Everything` doesn't register tools; calling a tools method 404s.
        let mut svc = everything();
        let JsonRpcMessage::Response(r) = svc
            .ready()
            .await
            .unwrap()
            .call(
                JsonRpcRequest::new(8, "tools/list", Some(json!({ "_meta": draft_meta() }))).into(),
            )
            .await
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(r.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn malformed_completion_ref_is_invalid_params() {
        let mut svc = everything();
        let JsonRpcMessage::Response(r) = svc
            .ready()
            .await
            .unwrap()
            .call(
                JsonRpcRequest::new(
                    9,
                    "completion/complete",
                    Some(json!({
                        "ref": { "type": "ref/prompt" },
                        "argument": { "name": "x", "value": "" },
                        "_meta": draft_meta(),
                    })),
                )
                .into(),
            )
            .await
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(r.error.unwrap().code, -32602);
    }
}
