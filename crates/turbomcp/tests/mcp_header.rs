//! Phase 8e: the `#[mcp_header]` round-trip (P4-4). A tool parameter marked
//! `#[mcp_header]` mirrors to an `Mcp-Param-*` HTTP header; the server folds the
//! header back into the call's `arguments`. Proven two ways:
//!  1. a raw client sending the param via header *only* (no body arg) — the
//!     server merge delivers it to the handler;
//!  2. the typed `Client`, which (after `list_tools` learns the schema mark)
//!     transparently mirrors the param to a header on `call_tool`.

#![cfg(all(feature = "client", feature = "http"))]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde_json::{Map, Value, json};
use turbomcp::CancellationToken;
use turbomcp::client::{ClientBuilder, ConnectMode, connect_http};
use turbomcp::http::{HttpConfig, ServeHttp};
use turbomcp::prelude::*;

#[derive(Clone)]
struct Geo;

#[server(name = "geo", version = "1.0.0")]
impl Geo {
    /// Locate a city within a region; `region` rides an HTTP header.
    #[tool(description = "Locate a city")]
    async fn locate(&self, city: String, #[mcp_header] region: String) -> McpResult<String> {
        Ok(format!("{city}@{region}"))
    }
}

async fn spawn_server() -> (String, CancellationToken) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);
    let shutdown = CancellationToken::new();
    let config = HttpConfig::new().with_shutdown(shutdown.clone());
    tokio::spawn(Geo.into_server().run_http(addr, config));
    tokio::time::sleep(Duration::from_millis(100)).await;
    (format!("http://{addr}/mcp"), shutdown)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_merges_header_param_supplied_only_via_header() {
    let (url, shutdown) = spawn_server().await;
    let http = reqwest::Client::new();

    // `region` is NOT in the body arguments — only in the Mcp-Param-region header.
    let resp = http
        .post(&url)
        .header("Mcp-Param-region", "us-west")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "locate",
                "arguments": { "city": "SF" },
                "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" }
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["content"][0]["text"], "SF@us-west");
    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_client_mirrors_marked_param_to_header() {
    let (url, shutdown) = spawn_server().await;
    let client = connect_http(
        ClientBuilder::new("c", "1.0.0").with_connect_mode(ConnectMode::Modern),
        &url,
    )
    .await
    .expect("connect");

    // list_tools teaches the client that `region` is a #[mcp_header] param.
    let tools = client.list_tools(None).await.expect("list_tools");
    assert!(tools.tools.iter().any(|t| t.name == "locate"));

    // call_tool now mirrors `region` to Mcp-Param-region (value also in body);
    // the server merge + handler produce the combined result.
    let mut args = Map::new();
    args.insert("city".into(), json!("SF"));
    args.insert("region".into(), json!("us-west"));
    let result = client.call_tool("locate", args).await.expect("call_tool");
    assert!(matches!(&result.content[0], neutral::Content::Text(t) if t == "SF@us-west"));
    shutdown.cancel();
}
