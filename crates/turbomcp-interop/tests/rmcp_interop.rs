//! Phase 8f: cross-SDK interop between turbomcp and the official Rust MCP SDK
//! (`rmcp`), both directions, in-process over a duplex pipe.
//!
//! rmcp's latest protocol version is `2025-11-25` — turbomcp's legacy path — so
//! interop runs there. Both SDKs frame newline-delimited JSON over an
//! `AsyncRead + AsyncWrite` stream, so a `tokio::io::duplex` connects them
//! directly with no subprocess.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ProtocolVersion,
    ServerCapabilities, ServerInfo,
};
use rmcp::{
    ErrorData as RmcpError, ServerHandler, ServiceExt, object, schemars, tool, tool_handler,
    tool_router,
};

use tokio::io::{BufReader, split};
use turbomcp::client::{ClientBuilder, ConnectMode};
use turbomcp::prelude::*;
use turbomcp::{LegacySessionAdapter, SerdeJsonCodec, serve};
use turbomcp_transport_stdio::LineTransport;

// ---- an rmcp server with one `add` tool ------------------------------------

#[derive(Clone)]
struct RmcpAdder {
    // Read by the `#[tool_handler]`-generated code, not visible to dead-code analysis.
    #[allow(dead_code)]
    tool_router: ToolRouter<RmcpAdder>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AddArgs {
    a: i32,
    b: i32,
}

#[tool_router]
impl RmcpAdder {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Add two integers")]
    fn add(
        &self,
        Parameters(AddArgs { a, b }): Parameters<AddArgs>,
    ) -> Result<CallToolResult, RmcpError> {
        Ok(CallToolResult::success(vec![Content::text(
            (a + b).to_string(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for RmcpAdder {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
    }
}

// ---- a turbomcp server with one `add` tool --------------------------------

#[derive(Clone)]
struct TurboAdder;

#[server(name = "turbo-adder", version = "1.0.0")]
impl TurboAdder {
    /// Add two integers.
    #[tool(description = "Add two integers")]
    async fn add(&self, a: i64, b: i64) -> McpResult<String> {
        Ok((a + b).to_string())
    }
}

/// Pull the first text block out of an rmcp `CallToolResult` via its JSON form
/// (robust against accessor-API churn).
fn rmcp_text(result: &CallToolResult) -> String {
    let v = serde_json::to_value(result).expect("serialize rmcp result");
    v["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turbomcp_client_drives_rmcp_server() {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);

    // rmcp server on one end.
    let (s_rd, s_wr) = split(server_io);
    let server = tokio::spawn(async move {
        let running = RmcpAdder::new().serve((s_rd, s_wr)).await?;
        running.waiting().await?;
        anyhow::Ok(())
    });

    // turbomcp client on the other end (legacy = rmcp's protocol version).
    let (c_rd, c_wr) = split(client_io);
    let transport = LineTransport::new(BufReader::new(c_rd), c_wr, SerdeJsonCodec);
    let client = ClientBuilder::new("turbomcp-client", "1.0.0")
        .with_connect_mode(ConnectMode::Legacy)
        .connect(transport)
        .await
        .expect("turbomcp handshakes with rmcp server");

    // The handshake negotiated against the real SDK and surfaced its identity.
    assert!(
        client.server_info().is_some(),
        "rmcp advertised server info"
    );

    let tools = client.list_tools(None).await.expect("list_tools");
    assert!(tools.tools.iter().any(|t| t.name == "add"));

    let mut args = serde_json::Map::new();
    args.insert("a".into(), serde_json::json!(2));
    args.insert("b".into(), serde_json::json!(3));
    let result = client.call_tool("add", args).await.expect("call_tool");
    assert!(matches!(&result.content[0], neutral::Content::Text(t) if t == "5"));

    drop(client); // closes the pipe; the rmcp server's `waiting()` returns.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rmcp_client_drives_turbomcp_server() {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);

    // turbomcp dual-stack server on one end (rmcp speaks the legacy path).
    let (s_rd, s_wr) = split(server_io);
    let transport = LineTransport::new(BufReader::new(s_rd), s_wr, SerdeJsonCodec);
    let service = LegacySessionAdapter::new(TurboAdder.into_server().build());
    tokio::spawn(serve(transport, service));

    // rmcp client on the other end (`()` is the no-op client handler).
    let client = ().serve(client_io).await.expect("rmcp handshakes with turbomcp server");

    let tools = client.list_all_tools().await.expect("list_all_tools");
    assert!(tools.iter().any(|t| t.name == "add"));

    let result = client
        .call_tool(CallToolRequestParams::new("add").with_arguments(object!({ "a": 2, "b": 3 })))
        .await
        .expect("rmcp call_tool against turbomcp");
    assert_eq!(rmcp_text(&result), "5");

    let _ = client.cancel().await;
}
