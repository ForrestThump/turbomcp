//! Phase 9d exit criterion: the draft Tasks extension end-to-end through the
//! real [`Client`] over stdio — `server/discover` reports the extension, the
//! client opts in (declares the capability), a tasked `tools/call` returns a
//! `CreateTaskResult` it polls to completion, and Tasks composes with MRTR
//! elicitation on the same server.

#![cfg(all(feature = "client", feature = "ext-tasks"))]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{BufReader, split};
use turbomcp::client::{Client, ClientBuilder, ClientHandler, ConnectMode, async_trait};
use turbomcp::ext_tasks::{EXTENSION_ID, TasksExtension};
use turbomcp::prelude::*;
use turbomcp::{LegacySessionAdapter, SerdeJsonCodec, serve};
use turbomcp_transport_stdio::LineTransport;

#[derive(Clone)]
struct Workshop;

#[server(name = "workshop", version = "1.0.0")]
impl Workshop {
    /// A slow report generator — run as a task.
    #[tool(description = "Generate a report")]
    async fn generate_report(&self, topic: String) -> McpResult<String> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(format!("Report: {topic}"))
    }

    /// Delete a path after eliciting confirmation (MRTR) — NOT a task.
    #[tool(description = "Delete a path after confirmation")]
    async fn delete(&self, ctx: &CallToolContext, path: String) -> McpResult<String> {
        let outcome = ctx
            .client
            .elicit(
                "confirm_delete",
                neutral::ElicitParams::new(
                    format!("Really delete {path}?"),
                    json!({
                        "type": "object",
                        "properties": { "ok": { "type": "boolean" } },
                        "required": ["ok"],
                    }),
                ),
            )
            .await?;
        if outcome.accepted() && outcome.content.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(format!("deleted {path}"))
        } else {
            Ok(format!("kept {path}"))
        }
    }
}

/// Confirms every elicitation (for the MRTR composition test).
struct Confirm;

#[async_trait]
impl ClientHandler for Confirm {
    async fn elicit(&self, _req: neutral::ElicitParams) -> neutral::ElicitOutcome {
        let mut content = Map::new();
        content.insert("ok".into(), json!(true));
        neutral::ElicitOutcome::new(neutral::ElicitAction::Accept, content)
    }
}

/// A client (declaring both the tasks extension and elicitation) connected to a
/// Workshop server that runs `generate_report` as a task.
async fn connect() -> Client {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);

    let (s_rd, s_wr) = split(server_io);
    let transport = LineTransport::new(BufReader::new(s_rd), s_wr, SerdeJsonCodec);
    let dispatcher = Workshop
        .into_server()
        .with_extension(Arc::new(
            TasksExtension::new()
                .task_tools(["generate_report"])
                .poll_interval_ms(Some(10)),
        ))
        .build();
    tokio::spawn(serve(transport, LegacySessionAdapter::new(dispatcher)));

    let (c_rd, c_wr) = split(client_io);
    let client_transport = LineTransport::new(BufReader::new(c_rd), c_wr, SerdeJsonCodec);
    ClientBuilder::new("workshop-client", "1.0.0")
        .with_connect_mode(ConnectMode::Modern)
        .with_capabilities(json!({
            "elicitation": {},
            "extensions": { EXTENSION_ID: {} },
        }))
        .with_handler(Confirm)
        .connect(client_transport)
        .await
        .expect("handshake")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discover_reports_the_extension() {
    let client = connect().await;
    // The modern handshake ran `server/discover`; the extension is advertised.
    let caps = client.server_capabilities();
    assert!(
        caps["extensions"].get(EXTENSION_ID).is_some(),
        "discover should advertise the tasks extension, got {caps}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasked_tool_returns_a_task_handle_polled_to_completion() {
    let client = connect().await;

    // The client opted into the extension, so the server returns a task handle.
    let mut args = Map::new();
    args.insert("topic".into(), json!("uptime"));
    let created = client
        .request("tools/call", {
            let mut p = Map::new();
            p.insert("name".into(), json!("generate_report"));
            p.insert("arguments".into(), Value::Object(args));
            p
        })
        .await
        .expect("tools/call");
    assert_eq!(created["resultType"], "task", "got {created}");
    let task_id = created["taskId"].as_str().expect("taskId").to_owned();

    // Poll tasks/get until the task completes, then read its result.
    let mut terminal = None;
    for _ in 0..200 {
        let mut p = Map::new();
        p.insert("taskId".into(), json!(task_id));
        let got = client.request("tasks/get", p).await.expect("tasks/get");
        if got["status"] == "completed" {
            terminal = Some(got);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let terminal = terminal.expect("task should complete");
    assert_eq!(terminal["resultType"], "complete");
    assert_eq!(terminal["result"]["content"][0]["text"], "Report: uptime");
    assert_eq!(terminal["result"]["isError"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mrtr_elicitation_composes_with_the_tasks_extension() {
    // On the SAME extension-enabled server, a non-tasked tool still drives MRTR
    // elicitation through the typed client + handler — Tasks and MRTR coexist.
    let client = connect().await;
    let mut args = Map::new();
    args.insert("path".into(), json!("/tmp/x"));
    let result = client.call_tool("delete", args).await.expect("call_tool");
    match &result.content[0] {
        neutral::Content::Text(t) => assert_eq!(t, "deleted /tmp/x"),
        other => panic!("unexpected content {other:?}"),
    }
}
