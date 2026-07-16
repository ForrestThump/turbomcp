//! Bucket-A A7: the WebSocket transport drives a real macro server end-to-end —
//! `serve_websocket` accepts a connection and `ws::connect` round-trips a
//! `tools/call` over `ws://`.
#![cfg(feature = "websocket")]

use serde_json::json;
use turbomcp::prelude::*;
use turbomcp::{JsonRpcMessage, JsonRpcRequest, Transport};

#[derive(Clone)]
struct Srv;

#[server(name = "ws-srv", version = "1.0.0")]
impl Srv {
    /// Echo the message back.
    #[tool(description = "Echo")]
    async fn echo(&self, msg: String) -> String {
        msg
    }
}

#[tokio::test]
async fn websocket_round_trip() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dispatcher = Srv.into_server().build();
    tokio::spawn(async move {
        let _ = turbomcp::ws::serve_websocket(listener, move || dispatcher.clone()).await;
    });

    let mut client = turbomcp::ws::connect(&format!("ws://{addr}"))
        .await
        .unwrap();
    let req = JsonRpcRequest::new(
        1,
        "tools/call",
        Some(json!({
            "name": "echo",
            "arguments": { "msg": "hi" },
            "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" },
        })),
    );
    client.send(JsonRpcMessage::Request(req)).await.unwrap();

    let JsonRpcMessage::Response(r) = client.recv().await.unwrap().expect("a response") else {
        panic!("expected a response")
    };
    assert_eq!(r.result.expect("result")["content"][0]["text"], "hi");
}
