//! TCP + Unix-socket transports: the stdio line framing over network sockets,
//! with one legacy session per connection (the `ServeNet`/`serve_tcp` factory
//! contract).

use serde_json::json;
use turbomcp::prelude::*;
use turbomcp::{JsonRpcMessage, JsonRpcRequest, LegacySessionAdapter, Transport};

#[derive(Clone)]
struct Srv;

#[server(name = "net-srv", version = "1.0.0")]
impl Srv {
    /// Echo the message back.
    #[tool(description = "Echo")]
    async fn echo(&self, msg: String) -> String {
        msg
    }
}

async fn respond<T: Transport>(t: &mut T, req: JsonRpcRequest) -> turbomcp::JsonRpcResponse {
    t.send(JsonRpcMessage::Request(req)).await.ok().unwrap();
    match t.recv().await.ok().flatten().expect("a response") {
        JsonRpcMessage::Response(r) => r,
        other => panic!("expected a response, got {other:?}"),
    }
}

fn initialize_req(id: i64) -> JsonRpcRequest {
    JsonRpcRequest::new(
        id,
        "initialize",
        Some(json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "net-client", "version": "1" },
        })),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_round_trip_both_versions() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dispatcher = Srv.into_server().build();
    tokio::spawn(async move {
        let _ = turbomcp::net::serve_tcp(listener, move || {
            LegacySessionAdapter::new(dispatcher.clone())
        })
        .await;
    });

    let mut client = turbomcp::net::connect_tcp(addr).await.unwrap();

    // Legacy path: initialize, then call.
    let init = respond(&mut client, initialize_req(0)).await;
    assert_eq!(
        init.result.expect("init result")["protocolVersion"],
        "2025-11-25"
    );
    let call = respond(
        &mut client,
        JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({ "name": "echo", "arguments": { "msg": "over tcp" } })),
        ),
    )
    .await;
    assert_eq!(
        call.result.expect("call result")["content"][0]["text"],
        "over tcp"
    );

    // Draft path on the same connection: stateless, version in _meta.
    let draft = respond(
        &mut client,
        JsonRpcRequest::new(
            2,
            "tools/call",
            Some(json!({
                "name": "echo",
                "arguments": { "msg": "draft" },
                "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" },
            })),
        ),
    )
    .await;
    assert_eq!(
        draft.result.expect("draft result")["content"][0]["text"],
        "draft"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_connections_get_independent_sessions() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dispatcher = Srv.into_server().build();
    tokio::spawn(async move {
        let _ = turbomcp::net::serve_tcp(listener, move || {
            LegacySessionAdapter::new(dispatcher.clone())
        })
        .await;
    });

    // Connection A initializes.
    let mut a = turbomcp::net::connect_tcp(addr).await.unwrap();
    let init = respond(&mut a, initialize_req(0)).await;
    assert!(init.error.is_none());

    // Connection B never initialized — its version-less request must be
    // refused (-32022: no negotiated version on THIS connection). If
    // connections shared one adapter, B would inherit A's session + version
    // and the call would succeed.
    let mut b = turbomcp::net::connect_tcp(addr).await.unwrap();
    let list = respond(&mut b, JsonRpcRequest::new(1, "tools/list", None)).await;
    let err = list.error.expect("uninitialized connection is refused");
    assert_eq!(err.code, -32022, "got {err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_line_does_not_take_down_the_server() {
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dispatcher = Srv.into_server().build();
    tokio::spawn(async move {
        let _ = turbomcp::net::serve_tcp(listener, move || {
            LegacySessionAdapter::new(dispatcher.clone())
        })
        .await;
    });

    // A raw connection floods 512 KiB with no newline, then hangs. The server's
    // per-frame cap must abort *that* connection without buffering it forever.
    let mut flooder = tokio::net::TcpStream::connect(addr).await.unwrap();
    flooder.write_all(&vec![b'a'; 512 * 1024]).await.unwrap();
    flooder.flush().await.unwrap();

    // A well-behaved connection still gets served — the flood took down only
    // its own connection, not the accept loop.
    let mut client = turbomcp::net::connect_tcp(addr).await.unwrap();
    let init = respond(&mut client, initialize_req(0)).await;
    assert!(
        init.error.is_none(),
        "server still serving other connections"
    );
    let call = respond(
        &mut client,
        JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({ "name": "echo", "arguments": { "msg": "alive" } })),
        ),
    )
    .await;
    assert_eq!(call.result.expect("result")["content"][0]["text"], "alive");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_socket_round_trip() {
    let dir = std::env::temp_dir().join(format!("turbomcp-net-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mcp.sock");
    let _ = std::fs::remove_file(&path);

    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let dispatcher = Srv.into_server().build();
    tokio::spawn(async move {
        let _ = turbomcp::net::serve_unix(listener, move || {
            LegacySessionAdapter::new(dispatcher.clone())
        })
        .await;
    });

    let mut client = turbomcp::net::connect_unix(&path).await.unwrap();
    let init = respond(&mut client, initialize_req(0)).await;
    assert!(init.error.is_none());
    let call = respond(
        &mut client,
        JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({ "name": "echo", "arguments": { "msg": "over unix" } })),
        ),
    )
    .await;
    assert_eq!(
        call.result.expect("call result")["content"][0]["text"],
        "over unix"
    );

    let _ = std::fs::remove_file(&path);
}
