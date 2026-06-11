//! `subscriptions/listen` over HTTP: the POST answers a long-lived SSE stream
//! — acknowledged event first, then opted-in notifications, keep-alive
//! comments between (the "survives a buffering proxy" exit piece), JSON error
//! bodies for rejected listens, and pruning when a stream is dropped.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use turbomcp4_core::{Implementation, McpResult};
use turbomcp4_protocol::neutral;
use turbomcp4_server::{
    CallToolContext, ListToolsContext, McpServerCore, MethodRouter, ServerNotifier,
    VersionDispatcher, WithTools,
};
use turbomcp4_transport_http::{HttpConfig, router};

#[derive(Clone)]
struct Watched;

impl McpServerCore for Watched {
    fn server_info(&self) -> Implementation {
        Implementation::new("watched", "0.1.0")
    }
}

impl WithTools for Watched {
    async fn list_tools(
        &self,
        _ctx: &ListToolsContext,
        _params: neutral::ListParams,
    ) -> McpResult<neutral::ListToolsResult> {
        Ok(neutral::ListToolsResult::new(vec![]))
    }

    async fn call_tool(
        &self,
        _ctx: &CallToolContext,
        _params: neutral::CallToolParams,
    ) -> McpResult<neutral::CallToolResult> {
        Ok(neutral::CallToolResult::text("ok"))
    }
}

/// One app + the notifier that publishes into it.
fn app(config: HttpConfig) -> (axum::Router, ServerNotifier) {
    let dispatcher = VersionDispatcher::new(Watched, MethodRouter::new().with_tools());
    let notifier = dispatcher.notifier();
    (router(dispatcher, config), notifier)
}

fn listen_request(id: i64, notifications: Value) -> Request<Body> {
    let body = json!({
        "jsonrpc": "2.0", "id": id, "method": "subscriptions/listen",
        "params": {
            "_meta": { "io.modelcontextprotocol/protocolVersion": "DRAFT-2026-v1" },
            "notifications": notifications,
        }
    });
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Pull raw chunks off the SSE body until one complete event (terminated by a
/// blank line) is buffered; returns it (comments included).
async fn next_sse_chunk(body: &mut Body, buffer: &mut String) -> String {
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let event: String = buffer.drain(..end + 2).collect();
            return event;
        }
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("an SSE frame should arrive")
            .expect("stream open")
            .expect("frame ok");
        if let Some(data) = frame.data_ref() {
            buffer.push_str(&String::from_utf8_lossy(data));
        }
    }
}

/// The `data:` payload of an SSE event, parsed as JSON.
fn event_json(event: &str) -> Value {
    let data: String = event
        .lines()
        .filter_map(|l| l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:")))
        .collect();
    serde_json::from_str(&data).expect("SSE data is one JSON-RPC message")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listen_answers_sse_with_ack_first_then_events() {
    let (app, notifier) = app(HttpConfig::new());
    let resp = app
        .oneshot(listen_request(7, json!({ "toolsListChanged": true })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"),
        "listen answers an SSE stream"
    );
    assert_eq!(
        resp.headers()["x-accel-buffering"],
        "no",
        "proxies are told not to buffer"
    );

    let mut body = resp.into_body();
    let mut buffer = String::new();

    // First message on the stream: the acknowledgment.
    let ack = event_json(&next_sse_chunk(&mut body, &mut buffer).await);
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
    assert_eq!(
        ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "7"
    );
    assert_eq!(ack["params"]["notifications"]["toolsListChanged"], true);

    // A published change arrives as the next event.
    notifier.tools_list_changed();
    let event = event_json(&next_sse_chunk(&mut body, &mut buffer).await);
    assert_eq!(event["method"], "notifications/tools/list_changed");
    assert_eq!(
        event["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "7"
    );
}

/// The proxy-survival exit piece: with no events flowing, keep-alive comments
/// keep the stream non-idle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_stream_emits_keepalive_comments() {
    let (app, _notifier) = app(HttpConfig::new().sse_keepalive(Duration::from_millis(50)));
    let resp = app
        .oneshot(listen_request(1, json!({ "toolsListChanged": true })))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let mut buffer = String::new();

    // Ack first, then — with zero events published — comment frames.
    let ack = next_sse_chunk(&mut body, &mut buffer).await;
    assert!(ack.contains("subscriptions/acknowledged"));
    let comment = next_sse_chunk(&mut body, &mut buffer).await;
    assert!(
        comment.starts_with(':') && comment.contains("keep-alive"),
        "expected a keep-alive comment, got: {comment:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_listen_filter_answers_json_error() {
    let (app, _notifier) = app(HttpConfig::new());
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "subscriptions/listen",
        "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "DRAFT-2026-v1" } }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json"),
        "rejected listens answer plain JSON, not a stream"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_stream_is_pruned_and_later_streams_keep_working() {
    let (app, notifier) = app(HttpConfig::new());

    // Open a stream and immediately drop it (client disconnect).
    let dropped = app
        .clone()
        .oneshot(listen_request(1, json!({ "toolsListChanged": true })))
        .await
        .unwrap();
    drop(dropped);

    // A second subscription still gets exactly its own events.
    let resp = app
        .oneshot(listen_request(2, json!({ "toolsListChanged": true })))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let mut buffer = String::new();
    let _ack = next_sse_chunk(&mut body, &mut buffer).await;

    notifier.tools_list_changed();
    let event = event_json(&next_sse_chunk(&mut body, &mut buffer).await);
    assert_eq!(event["method"], "notifications/tools/list_changed");
    assert_eq!(
        event["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "2"
    );
}
