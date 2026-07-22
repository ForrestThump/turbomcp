//! `connect_child` smoke test: spawn the `hello_world` example as a real
//! subprocess, run the handshake over its stdio, exercise one tool call, and
//! tear the child down. This is the common local-MCP deployment shape — a
//! client that owns the server process — end to end.
#![cfg(feature = "client")]

use serde_json::{Map, json};
use tokio::process::Command;
use turbomcp::client::{ClientBuilder, connect_child};

/// The `hello_world` example binary, built alongside this test by `cargo
/// test` (same package ⇒ same target dir: `…/debug/examples/hello_world`).
fn hello_world_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop(); // …/debug/deps
    path.pop(); // …/debug
    path.push("examples");
    path.push(format!("hello_world{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "example binary not built at {}",
        path.display()
    );
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_child_spawns_handshakes_and_calls() {
    let (client, mut child) = connect_child(
        ClientBuilder::new("child-smoke", "1.0.0"),
        Command::new(hello_world_bin()),
    )
    .await
    .expect("spawn + handshake");

    assert_eq!(client.server_info().expect("server info").name, "hello");

    let tools = client.list_tools(None).await.expect("list_tools");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "hello");

    let mut args = Map::new();
    args.insert("name".into(), json!("world"));
    let result = client.call_tool("hello", args).await.expect("call_tool");
    match &result.content[0] {
        turbomcp::neutral::Content::Text { text, .. } => assert_eq!(text, "Hello, world!"),
        other => panic!("expected text content, got {other:?}"),
    }

    child.kill().await.expect("child teardown");
}
