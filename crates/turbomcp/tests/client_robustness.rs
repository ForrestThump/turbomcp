//! Client failure semantics against misbehaving servers: a dropped pipe fails
//! pending requests with `Closed` (promptly — no hang, no timeout wait), a
//! silent server yields `Timeout`, a response bearing an unknown id is ignored
//! without disturbing correlation, and a garbage frame ends the connection
//! (pending requests again fail `Closed`).

#![cfg(feature = "client")]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, split};
use turbomcp::SerdeJsonCodec;
use turbomcp::client::{Client, ClientBuilder, ClientError, ConnectMode};
use turbomcp_transport_stdio::LineTransport;

/// What the scripted server does when the client's `tools/list` arrives.
#[derive(Clone, Copy)]
enum OnList {
    /// Close the connection without answering.
    DropPipe,
    /// Never answer (but keep the connection open).
    StaySilent,
    /// Write a response with an id nobody asked for, then the real answer.
    UnknownIdFirst,
    /// Write a non-JSON line.
    Garbage,
}

/// A hand-scripted draft server: answers `server/discover`, then applies
/// `behavior` to the first `tools/list`.
fn spawn_scripted_server(server_io: tokio::io::DuplexStream, behavior: OnList) {
    tokio::spawn(async move {
        let (rd, mut wr) = split(server_io);
        let mut lines = BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let frame: Value = serde_json::from_str(&line).expect("valid json from client");
            let id = frame.get("id").cloned().unwrap_or(Value::Null);
            let result = match frame.get("method").and_then(Value::as_str) {
                Some("server/discover") => json!({
                    "capabilities": { "tools": {} },
                    "supportedVersions": ["2026-07-28"],
                    "resultType": "complete", "cacheScope": "private", "ttlMs": 0
                }),
                Some("tools/list") => match behavior {
                    OnList::DropPipe => return, // drops rd + wr: EOF on both halves
                    OnList::StaySilent => continue,
                    OnList::Garbage => {
                        wr.write_all(b"!!! not json !!!\n").await.unwrap();
                        continue;
                    }
                    OnList::UnknownIdFirst => {
                        let stray = json!({
                            "jsonrpc": "2.0", "id": 424_242,
                            "result": { "should": "be ignored" }
                        });
                        wr.write_all(format!("{stray}\n").as_bytes()).await.unwrap();
                        json!({ "tools": [], "resultType": "complete",
                                "cacheScope": "private", "ttlMs": 0 })
                    }
                },
                other => panic!("unexpected method from client: {other:?}"),
            };
            let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            wr.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        }
    });
}

async fn connect(behavior: OnList, request_timeout: Duration) -> Client {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_scripted_server(server_io, behavior);

    let (c_rd, c_wr) = split(client_io);
    let transport = LineTransport::new(BufReader::new(c_rd), c_wr, SerdeJsonCodec);
    ClientBuilder::new("robustness", "1.0.0")
        .with_connect_mode(ConnectMode::Modern)
        .with_timeout(request_timeout)
        .connect(transport)
        .await
        .expect("handshake succeeds")
}

/// The whole point of the actor's exit-drain: a caller blocked on a request
/// must see `Closed` as soon as the connection dies — not hang, and not wait
/// out the (long) request timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_pipe_fails_pending_requests_closed_promptly() {
    let client = connect(OnList::DropPipe, Duration::from_secs(60)).await;
    let result = tokio::time::timeout(Duration::from_secs(2), client.list_tools(None))
        .await
        .expect("failure arrives promptly, not after the 60s request timeout");
    assert!(
        matches!(result, Err(ClientError::Closed)),
        "expected Closed, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_server_yields_timeout() {
    let client = connect(OnList::StaySilent, Duration::from_millis(150)).await;
    let result = client.list_tools(None).await;
    assert!(
        matches!(result, Err(ClientError::Timeout)),
        "expected Timeout, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_response_id_is_ignored_and_correlation_survives() {
    let client = connect(OnList::UnknownIdFirst, Duration::from_secs(5)).await;
    let tools = client
        .list_tools(None)
        .await
        .expect("the stray response must not disturb the real one");
    assert!(tools.tools.is_empty());
}

/// A frame that fails to decode ends the connection (the transport is the
/// trust boundary — there is no resync on a corrupted stream) and pending
/// requests fail `Closed` rather than hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn garbage_frame_ends_the_connection_and_fails_pending() {
    let client = connect(OnList::Garbage, Duration::from_secs(60)).await;
    let result = tokio::time::timeout(Duration::from_secs(2), client.list_tools(None))
        .await
        .expect("failure arrives promptly");
    assert!(
        matches!(result, Err(ClientError::Closed)),
        "expected Closed, got {result:?}"
    );
    // The client object itself stays safe to use: further calls fail cleanly.
    let again = client.request("tools/list", Map::new()).await;
    assert!(again.is_err());
}
