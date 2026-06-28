//! The HTTP one-liner: `MyServer.into_server().run_http(addr, config)` builds
//! the dispatcher, serves it, AND auto-wires session termination — so a client
//! `DELETE` ends its session (204) without the user touching the terminator.
#![cfg(feature = "http")]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use turbomcp::CancellationToken;
use turbomcp::http::{HttpConfig, ServeHttp};
use turbomcp::prelude::*;

#[derive(Clone)]
struct Greeter;

#[server(name = "greeter", version = "0.1.0")]
impl Greeter {
    /// Greet someone.
    #[tool]
    async fn greet(&self, name: String) -> McpResult<String> {
        Ok(format!("Hello, {name}!"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_http_serves_and_auto_wires_delete_termination() {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);

    let shutdown = CancellationToken::new();
    let config = HttpConfig::new().with_shutdown(shutdown.clone());
    // The one-liner under test.
    let server = tokio::spawn(Greeter.into_server().run_http(addr, config));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let url = format!("http://{addr}/mcp");
    let client = reqwest::Client::new();

    // initialize → a 2025-11-25 session is minted in the Mcp-Session-Id header.
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "c", "version": "1" },
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .expect("session header")
        .to_str()
        .unwrap()
        .to_owned();

    // DELETE the session: run_http wired the terminator, so this is honored.
    let resp = client
        .delete(&url)
        .header("mcp-session-id", &sid)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    // The session is gone: a follow-up request 404s (re-initialize).
    let resp = client
        .post(&url)
        .header("mcp-session-id", &sid)
        .header("mcp-protocol-version", "2025-11-25")
        .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server shuts down")
        .unwrap()
        .expect("run_http exits Ok");
}
