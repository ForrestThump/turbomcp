//! Phase 8b exit criterion: the typed [`Client`] drives a macro-generated
//! `#[server]` over stdio framing, in **all three connect modes** — `Modern`
//! (stateless `server/discover`), `Legacy` (`initialize` handshake), and `Auto`
//! (which resolves to the modern path on a dual-stack server). The same
//! version-stable neutral API works regardless of the negotiated version.

#![cfg(feature = "client")]

use serde_json::{Map, json};
use tokio::io::{BufReader, split};
use turbomcp::client::{Client, ClientBuilder, ConnectMode};
use turbomcp::prelude::*;
use turbomcp::{LegacySessionAdapter, SerdeJsonCodec, serve};
use turbomcp_core::ProtocolVersion;
use turbomcp_transport_stdio::LineTransport;

#[derive(Clone)]
struct Demo;

#[server(name = "demo", version = "1.0.0")]
impl Demo {
    /// Shout a word back, upper-cased.
    #[tool(description = "Shout a word")]
    async fn shout(&self, word: String) -> McpResult<String> {
        Ok(word.to_uppercase())
    }

    /// A fixed greeting resource.
    #[resource("demo://greeting")]
    async fn greeting(&self) -> McpResult<String> {
        Ok("hello there".to_string())
    }

    /// A welcome prompt.
    #[prompt]
    async fn welcome(&self, name: String) -> McpResult<String> {
        Ok(format!("Welcome, {name}!"))
    }
}

/// Spawn the dual-stack server (the exact stack `run_stdio` wires) on one end of
/// a duplex pipe and connect a typed [`Client`] in `mode` on the other.
async fn connect(mode: ConnectMode) -> Client {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (s_rd, s_wr) = split(server_io);
    let transport = LineTransport::new(BufReader::new(s_rd), s_wr, SerdeJsonCodec);
    let service = LegacySessionAdapter::new(Demo.into_server().build());
    tokio::spawn(serve(transport, service));

    let (c_rd, c_wr) = split(client_io);
    let client_transport = LineTransport::new(BufReader::new(c_rd), c_wr, SerdeJsonCodec);
    ClientBuilder::new("test-client", "1.0.0")
        .with_connect_mode(mode)
        .connect(client_transport)
        .await
        .expect("handshake succeeds")
}

/// Drive the whole typed read API and assert results — shared across modes.
async fn exercise(client: &Client) {
    // Handshake surfaced the server identity.
    assert_eq!(client.server_info().map(|i| i.name.as_str()), Some("demo"));

    // ping
    client.ping().await.expect("ping ok");

    // tools/list + tools/call
    let tools = client.list_tools(None).await.expect("list_tools");
    assert!(tools.tools.iter().any(|t| t.name == "shout"));

    let mut args = Map::new();
    args.insert("word".into(), json!("hi"));
    let result = client.call_tool("shout", args).await.expect("call_tool");
    assert!(!result.is_error);
    assert!(matches!(&result.content[0], neutral::Content::Text(t) if t == "HI"));

    // resources/read
    let read = client
        .read_resource("demo://greeting")
        .await
        .expect("read_resource");
    assert!(
        matches!(&read.contents[0], neutral::ResourceContents::Text { text, .. } if text == "hello there")
    );

    // prompts/list + prompts/get
    let prompts = client.list_prompts(None).await.expect("list_prompts");
    assert!(prompts.prompts.iter().any(|p| p.name == "welcome"));

    let mut pargs = Map::new();
    pargs.insert("name".into(), json!("Ada"));
    let prompt = client
        .get_prompt("welcome", pargs)
        .await
        .expect("get_prompt");
    assert!(matches!(&prompt.messages[0].content, neutral::Content::Text(t) if t.contains("Ada")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modern_mode_negotiates_draft() {
    let client = connect(ConnectMode::Modern).await;
    assert_eq!(client.protocol_version(), &ProtocolVersion::Draft);
    exercise(&client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_mode_negotiates_2025() {
    let client = connect(ConnectMode::Legacy).await;
    assert_eq!(client.protocol_version(), &ProtocolVersion::V2025_11_25);
    exercise(&client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_mode_resolves_to_modern_on_dual_stack() {
    let client = connect(ConnectMode::Auto).await;
    // A dual-stack server answers server/discover, so Auto lands on the draft.
    assert_eq!(client.protocol_version(), &ProtocolVersion::Draft);
    exercise(&client).await;
}
