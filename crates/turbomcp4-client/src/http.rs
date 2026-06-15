//! The Streamable HTTP client transport (feature `http`).
//!
//! Streamable HTTP is request-scoped — each JSON-RPC request is its own POST —
//! yet the [`Connection`](crate::Connection) actor wants the persistent
//! [`Transport`] `send`/`recv` shape. This transport bridges the two: `send`
//! fires a POST in a spawned task whose response (a single `application/json`
//! frame, or a `text/event-stream` of frames) is funneled into an inbound
//! channel that `recv` drains. Response correlation happens one layer up (by
//! request id in the `Connection`), so the transport never has to match
//! requests to responses itself — it only has to deliver every inbound frame.
//!
//! The `Mcp-Session-Id` minted by a legacy `initialize` response is captured
//! and replayed on subsequent requests (the stateful path); the stateless draft
//! path simply never sets it.
//!
//! Per-request failures are scoped to that request: a POST that fails (network,
//! HTTP status, decode) synthesizes a JSON-RPC error *response* for the
//! request's id rather than tearing down the whole connection — only the one
//! waiting caller sees the error.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use tokio::sync::mpsc;
use turbomcp4_core::{JsonRpcError, JsonRpcMessage, JsonRpcResponse};
use turbomcp4_service::Transport;

use crate::client::{Client, ClientBuilder};
use crate::error::{ClientError, ClientResult};

/// Failures specific to the HTTP client transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HttpClientError {
    /// The transport's inbound channel closed (connection torn down).
    #[error("http client transport closed")]
    Closed,
}

const SESSION_HEADER: &str = "mcp-session-id";

/// Shared state for the spawned POST tasks: the HTTP client, target URL, the
/// captured session id, and the inbound delivery channel.
struct Shared {
    http: reqwest::Client,
    url: String,
    session: Mutex<Option<String>>,
    inbound_tx: mpsc::Sender<JsonRpcMessage>,
}

/// A Streamable HTTP transport to a single MCP endpoint URL.
pub struct HttpClientTransport {
    shared: Arc<Shared>,
    inbound_rx: mpsc::Receiver<JsonRpcMessage>,
}

impl HttpClientTransport {
    /// Build a transport targeting `url` (e.g. `http://127.0.0.1:8080/mcp`).
    ///
    /// # Errors
    /// [`ClientError::Protocol`] if the underlying HTTP client can't be built.
    pub fn new(url: impl Into<String>) -> ClientResult<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| ClientError::Protocol(format!("http client build failed: {e}")))?;
        let (inbound_tx, inbound_rx) = mpsc::channel(1024);
        Ok(Self {
            shared: Arc::new(Shared {
                http,
                url: url.into(),
                session: Mutex::new(None),
                inbound_tx,
            }),
            inbound_rx,
        })
    }
}

impl Transport for HttpClientTransport {
    type Error = HttpClientError;

    async fn send(&mut self, msg: JsonRpcMessage) -> Result<(), Self::Error> {
        // POST and pump the response in the background so the driver can keep
        // sending; HTTP requests are independent and may run concurrently.
        let shared = Arc::clone(&self.shared);
        tokio::spawn(post_and_pump(shared, msg));
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<JsonRpcMessage>, Self::Error> {
        // `None` here means every sender (the Shared in this transport + any
        // in-flight pump task) has dropped — a clean end-of-stream.
        Ok(self.inbound_rx.recv().await)
    }

    async fn close(self) -> Result<(), Self::Error> {
        // Best-effort session termination (the spec's explicit DELETE).
        let sid = self.shared.session.lock().expect("session mutex").clone();
        if let Some(sid) = sid {
            let _ = self
                .shared
                .http
                .delete(&self.shared.url)
                .header(SESSION_HEADER, sid)
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        }
        Ok(())
    }
}

/// POST `msg` and feed whatever comes back into the inbound channel.
async fn post_and_pump(shared: Arc<Shared>, msg: JsonRpcMessage) {
    // The request id (if this is a request) so a failure can be reported to just
    // this caller as an error response rather than killing the connection.
    let request_id = match &msg {
        JsonRpcMessage::Request(r) => Some(r.id.clone()),
        _ => None,
    };

    if let Err(err) = pump(&shared, msg).await {
        match request_id {
            Some(id) => {
                // Surface the failure to the one waiting caller.
                let resp = JsonRpcResponse::error(
                    id,
                    JsonRpcError {
                        code: -32001,
                        message: err,
                        data: None,
                    },
                );
                let _ = shared.inbound_tx.send(JsonRpcMessage::Response(resp)).await;
            }
            // Notifications / responses have no waiter — just log.
            None => tracing::debug!(error = %err, "http client POST failed (no waiter)"),
        }
    }
}

/// The fallible body of a POST + response pump. Errors are returned as a string
/// for [`post_and_pump`] to route.
async fn pump(shared: &Shared, mut msg: JsonRpcMessage) -> Result<(), String> {
    // `#[mcp_header]` mirroring: lift the marked params out of the `_meta` signal
    // into `Mcp-Param-*` headers (their values stay in `arguments`). Done before
    // serialization so the signal never reaches the wire body.
    let header_params = extract_header_params(&mut msg);

    let body = serde_json::to_string(&msg).map_err(|e| format!("encode failed: {e}"))?;

    let mut req = shared
        .http
        .post(&shared.url)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(body);
    if let Some(sid) = shared.session.lock().expect("session mutex").clone() {
        req = req.header(SESSION_HEADER, sid);
    }
    for (name, value) in header_params {
        req = req.header(format!("Mcp-Param-{name}"), value);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    // Capture (or refresh) the session id from any response.
    if let Some(sid) = resp
        .headers()
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        *shared.session.lock().expect("session mutex") = Some(sid.to_string());
    }

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("http status {status}"));
    }

    let is_sse = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));

    if is_sse {
        // Each SSE `data:` event is one JSON-RPC frame; the stream ends after the
        // final response. Deliver every frame — the Connection correlates them.
        let mut events = resp.bytes_stream().eventsource();
        while let Some(event) = events.next().await {
            let event = event.map_err(|e| format!("sse stream error: {e}"))?;
            if event.data.is_empty() {
                continue; // keep-alive comment / empty event
            }
            let frame: JsonRpcMessage = serde_json::from_str(&event.data)
                .map_err(|e| format!("sse frame decode failed: {e}"))?;
            if shared.inbound_tx.send(frame).await.is_err() {
                break; // receiver gone
            }
        }
    } else {
        // application/json: a single response frame (202 with no body → nothing).
        let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
        if text.trim().is_empty() {
            return Ok(());
        }
        let frame: JsonRpcMessage =
            serde_json::from_str(&text).map_err(|e| format!("json decode failed: {e}"))?;
        let _ = shared.inbound_tx.send(frame).await;
    }
    Ok(())
}

/// Pull the `#[mcp_header]` mirror signal out of a request's `_meta` and resolve
/// each named param to its `(name, string-value)` from `arguments`.
///
/// Mutates `msg`: removes the [`HEADER_PARAMS_META_KEY`](crate::client::HEADER_PARAMS_META_KEY)
/// entry so it never reaches the wire. The param values stay in `arguments`
/// (mirroring — the header is an HTTP-visible copy, not a move).
fn extract_header_params(msg: &mut JsonRpcMessage) -> Vec<(String, String)> {
    let JsonRpcMessage::Request(req) = msg else {
        return Vec::new();
    };
    let Some(params) = req
        .params
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Vec::new();
    };

    // Take the list of header-param names out of `_meta`.
    let names: Vec<String> = params
        .get_mut("_meta")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|meta| meta.remove(crate::client::HEADER_PARAMS_META_KEY))
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    if names.is_empty() {
        return Vec::new();
    }

    let Some(args) = params
        .get("arguments")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    names
        .into_iter()
        .filter_map(|name| {
            args.get(&name).map(|v| {
                let value = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (name, value)
            })
        })
        .collect()
}

/// Connect a [`Client`] to an MCP server over Streamable HTTP at `url`, running
/// the handshake.
///
/// # Errors
/// Propagates transport construction and handshake failures.
pub async fn connect_http(builder: ClientBuilder, url: impl Into<String>) -> ClientResult<Client> {
    let transport = HttpClientTransport::new(url)?;
    builder.connect(transport).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turbomcp4_core::JsonRpcRequest;

    #[test]
    fn extracts_marked_header_params_and_strips_the_signal() {
        let mut msg = JsonRpcMessage::Request(JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({
                "name": "locate",
                "arguments": { "city": "SF", "region": "us-west", "n": 3 },
                "_meta": {
                    crate::client::HEADER_PARAMS_META_KEY: ["region", "n"],
                    "io.modelcontextprotocol/protocolVersion": "DRAFT-2026-v1",
                },
            })),
        ));

        let headers = extract_header_params(&mut msg);
        // String value passed through verbatim; non-string serialized as JSON.
        assert_eq!(
            headers,
            vec![
                ("region".to_owned(), "us-west".to_owned()),
                ("n".to_owned(), "3".to_owned())
            ]
        );

        // The signal is stripped; the values remain in `arguments`; other `_meta`
        // keys are untouched.
        let JsonRpcMessage::Request(req) = &msg else {
            unreachable!()
        };
        let params = req.params.as_ref().unwrap();
        assert!(
            params["_meta"]
                .get(crate::client::HEADER_PARAMS_META_KEY)
                .is_none()
        );
        assert_eq!(
            params["_meta"]["io.modelcontextprotocol/protocolVersion"],
            "DRAFT-2026-v1"
        );
        assert_eq!(params["arguments"]["region"], "us-west");
    }

    #[test]
    fn no_signal_yields_no_headers() {
        let mut msg = JsonRpcMessage::Request(JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({ "name": "x", "arguments": { "a": 1 } })),
        ));
        assert!(extract_header_params(&mut msg).is_empty());
    }
}
