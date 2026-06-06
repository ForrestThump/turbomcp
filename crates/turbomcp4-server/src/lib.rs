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
//! Phase 2 wires the modern `DRAFT-2026-v1` path end to end; the legacy
//! `2025-11-25` path is recognized and stubbed (Phase 5).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod context;
mod dispatcher;
mod router;
mod traits;

pub use context::{CallToolContext, ListToolsContext};
pub use dispatcher::VersionDispatcher;
pub use router::MethodRouter;
pub use traits::{McpServerCore, WithTools};

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
        async fn list_tools(&self, _ctx: &ListToolsContext) -> McpResult<neutral::ListToolsResult> {
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
    async fn legacy_version_is_stubbed() {
        let mut svc = dispatcher();
        let meta = json!({ "io.modelcontextprotocol/protocolVersion": "2025-11-25" });
        let req = JsonRpcRequest::new(5, "tools/list", Some(json!({ "_meta": meta })));
        let JsonRpcMessage::Response(r) = call(&mut svc, req).await else {
            panic!()
        };
        let err = r.error.expect("legacy path stubbed");
        assert!(err.message.contains("Phase 5"));
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
}
