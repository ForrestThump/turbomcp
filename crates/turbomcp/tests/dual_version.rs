//! Phase 5 exit criterion, as a test: a single macro-generated `#[server]`
//! type answers **both protocol versions** — the stateless `2026-07-28`
//! path and the stateful `2025-11-25` `initialize` handshake — over both the
//! stdio framing stack (with the [`LegacySessionAdapter`], exactly what
//! `run_stdio()` wires) and the Streamable HTTP endpoint (header-routed
//! dual-stack).

#![cfg(feature = "http")]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt; // oneshot
use turbomcp::LegacySessionAdapter;
use turbomcp::http::{HttpConfig, router};
use turbomcp::prelude::*;
use turbomcp::{SerdeJsonCodec, serve};
use turbomcp_transport_stdio::LineTransport;

#[derive(Clone)]
struct Demo;

#[server(name = "demo", version = "1.0.0")]
impl Demo {
    /// Echo a word back, upper-cased.
    #[tool(description = "Shout a word")]
    async fn shout(&self, word: String) -> McpResult<String> {
        Ok(word.to_uppercase())
    }
}

fn initialize_frame(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "legacy-client", "version": "1" },
        }
    })
}

/// A legacy call states no version anywhere in the body.
fn legacy_call_frame(id: u64, word: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "shout", "arguments": { "word": word } }
    })
}

/// A draft call carries the version in `_meta` per request.
fn draft_call_frame(id: u64, word: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {
            "name": "shout", "arguments": { "word": word },
            "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" }
        }
    })
}

#[tokio::test]
async fn both_versions_over_stdio_framing() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (server_rx, server_tx) = tokio::io::split(server);
    let transport = LineTransport::new(BufReader::new(server_rx), server_tx, SerdeJsonCodec);
    // The same stack `run_stdio()` builds: adapter around the dispatcher.
    let service = LegacySessionAdapter::new(Demo.into_server().build());
    let task = tokio::spawn(serve(transport, service));

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut client_rx = BufReader::new(client_rx);

    let mut send = async |frame: Value| {
        client_tx
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        client_tx.flush().await.unwrap();
    };
    macro_rules! recv {
        () => {{
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                client_rx.read_line(&mut line).await.unwrap();
            })
            .await
            .expect("a response should arrive");
            serde_json::from_str::<Value>(line.trim()).unwrap()
        }};
    }

    // Legacy handshake, then a version-less legacy call.
    send(initialize_frame(1)).await;
    let init = recv!();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init["result"]["serverInfo"]["name"], "demo");

    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;

    send(legacy_call_frame(2, "hi")).await;
    let legacy = recv!();
    assert_eq!(legacy["result"]["content"][0]["text"], "HI");
    assert!(
        legacy["result"].get("resultType").is_none(),
        "legacy wire must not carry the draft envelope"
    );

    // The SAME connection still serves a draft request that states its version.
    send(draft_call_frame(3, "bye")).await;
    let draft = recv!();
    assert_eq!(draft["result"]["content"][0]["text"], "BYE");
    assert_eq!(draft["result"]["resultType"], "complete");

    client_tx.shutdown().await.unwrap();
    task.await.unwrap().expect("serve loop ends cleanly on EOF");
}

#[tokio::test]
async fn both_versions_over_http() {
    let app = router(Demo.into_server().build(), HttpConfig::new());
    let post = |body: Value, headers: Vec<(&'static str, String)>| {
        let mut req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json");
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req.body(Body::from(body.to_string())).unwrap()
    };

    // Legacy: initialize mints the session header…
    let resp = app
        .clone()
        .oneshot(post(initialize_frame(1), vec![]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .expect("minted session")
        .to_str()
        .unwrap()
        .to_owned();

    // …and the session header routes the legacy call.
    let resp = app
        .clone()
        .oneshot(post(
            legacy_call_frame(2, "hi"),
            vec![
                ("mcp-session-id", sid),
                ("mcp-protocol-version", "2025-11-25".to_owned()),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["result"]["content"][0]["text"], "HI");
    assert!(v["result"].get("resultType").is_none());

    // Modern: stateless draft body, no headers, same app.
    let resp = app
        .clone()
        .oneshot(post(draft_call_frame(3, "bye"), vec![]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["result"]["content"][0]["text"], "BYE");
    assert_eq!(v["result"]["resultType"], "complete");
}
