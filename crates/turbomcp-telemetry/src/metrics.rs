//! [`MetricsLayer`] — a [`tower::Layer`] recording OpenTelemetry metrics for
//! each MCP request: a request counter, a duration histogram, and an
//! in-flight up-down counter.
//!
//! Like [`TraceContextLayer`](crate::TraceContextLayer) it is transport-
//! agnostic (it sees `JsonRpcMessage`) and composes around a dispatcher as
//! shared RPC middleware; stack the two together for traces *and* metrics.
//! Instruments are read from the global [`opentelemetry`] meter provider, so
//! the same OTLP pipeline (`otlp` feature, or any host-installed provider)
//! exports them.
//!
//! ## Label cardinality is bounded and PII-safe
//!
//! Every metric is labeled by `mcp.method` (a fixed method name), the negotiated
//! `mcp.protocol_version`, and an `outcome` of `ok`/`error` — all low-cardinality
//! and free of caller data. Identity is deliberately **not** a metric label
//! (it would be unbounded and would leak PII); identity lives on spans, redacted.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use opentelemetry::metrics::{Counter, Histogram, UpDownCounter};
use opentelemetry::{KeyValue, global};
use pin_project_lite::pin_project;
use tower::{Layer, Service};
use turbomcp_core::{JsonRpcMessage, ProtocolVersion, meta};

/// The instruments, built once and shared (cheap clones — the OTel handles are
/// `Arc`-backed).
#[derive(Clone)]
struct Instruments {
    requests: Counter<u64>,
    duration: Histogram<f64>,
    in_flight: UpDownCounter<i64>,
}

impl Instruments {
    fn new() -> Self {
        let meter = global::meter("turbomcp");
        Self {
            requests: meter
                .u64_counter("mcp.server.requests")
                .with_description("Total MCP requests handled.")
                .build(),
            duration: meter
                .f64_histogram("mcp.server.request.duration")
                .with_description("MCP request handling duration.")
                .with_unit("s")
                .build(),
            in_flight: meter
                .i64_up_down_counter("mcp.server.active_requests")
                .with_description("In-flight MCP requests.")
                .build(),
        }
    }
}

/// A [`tower::Layer`] recording per-request OpenTelemetry metrics. Compose it
/// around a dispatcher like any shared RPC middleware (typically alongside
/// [`TraceContextLayer`](crate::TraceContextLayer)).
#[derive(Clone)]
pub struct MetricsLayer {
    instruments: Arc<Instruments>,
}

impl MetricsLayer {
    /// Build the layer, reading instruments from the global meter provider.
    #[must_use]
    pub fn new() -> Self {
        Self {
            instruments: Arc::new(Instruments::new()),
        }
    }
}

impl Default for MetricsLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MetricsLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MetricsLayer")
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = Metrics<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Metrics {
            inner,
            instruments: Arc::clone(&self.instruments),
        }
    }
}

/// The service produced by [`MetricsLayer`].
#[derive(Clone)]
pub struct Metrics<S> {
    inner: S,
    instruments: Arc<Instruments>,
}

impl<S, E> Service<JsonRpcMessage> for Metrics<S>
where
    S: Service<JsonRpcMessage, Response = Option<JsonRpcMessage>, Error = E>,
{
    type Response = Option<JsonRpcMessage>;
    type Error = E;
    type Future = MetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: JsonRpcMessage) -> Self::Future {
        // Notifications and responses are not measured as "requests"; only true
        // requests carry a method worth a metric label.
        let base_labels = match &req {
            JsonRpcMessage::Request(r) => {
                let mut labels = vec![KeyValue::new("mcp.method", r.method.clone())];
                labels.push(KeyValue::new(
                    "mcp.protocol_version",
                    protocol_version_label(&req),
                ));
                Some(labels)
            }
            _ => None,
        };
        if let Some(labels) = &base_labels {
            self.instruments.in_flight.add(1, labels);
        }
        MetricsFuture {
            inner: self.inner.call(req),
            instruments: Arc::clone(&self.instruments),
            labels: base_labels,
            start: Instant::now(),
        }
    }
}

pin_project! {
    /// Times the inner future and records the request metrics on completion,
    /// decrementing the in-flight counter (even if the inner future errors).
    pub struct MetricsFuture<F> {
        #[pin]
        inner: F,
        instruments: Arc<Instruments>,
        // `None` for non-request messages (unmeasured).
        labels: Option<Vec<KeyValue>>,
        start: Instant,
    }
}

impl<F, E> Future for MetricsFuture<F>
where
    F: Future<Output = Result<Option<JsonRpcMessage>, E>>,
{
    type Output = Result<Option<JsonRpcMessage>, E>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let result = std::task::ready!(this.inner.poll(cx));

        if let Some(base) = this.labels.take() {
            let outcome = match &result {
                // A transport-level error, or a JSON-RPC error response, both
                // count as `error`; a successful/absent response is `ok`.
                Err(_) => "error",
                Ok(Some(JsonRpcMessage::Response(r))) if r.error.is_some() => "error",
                Ok(_) => "ok",
            };
            let elapsed = this.start.elapsed().as_secs_f64();
            let mut labels = base;
            labels.push(KeyValue::new("outcome", outcome));
            this.instruments.requests.add(1, &labels);
            this.instruments.duration.record(elapsed, &labels);
            // Decrement in-flight with the base labels (no outcome) it was
            // incremented with.
            let in_flight_labels = &labels[..labels.len() - 1];
            this.instruments.in_flight.add(-1, in_flight_labels);
        }
        Poll::Ready(result)
    }
}

/// The negotiated protocol version label from a request's `_meta`
/// (`unknown` when absent — e.g. `initialize`, whose version is in the body).
fn protocol_version_label(req: &JsonRpcMessage) -> &'static str {
    let JsonRpcMessage::Request(r) = req else {
        return "unknown";
    };
    match turbomcp_protocol_version(r.params.as_ref()) {
        Some(ProtocolVersion::V2025_11_25) => "2025-11-25",
        Some(ProtocolVersion::Draft) => "2026-07-28",
        Some(_) => "other",
        None => "unknown",
    }
}

/// Read `_meta.io.modelcontextprotocol/protocolVersion` without depending on
/// `turbomcp-protocol` (telemetry sits below it): the key is stable core meta.
fn turbomcp_protocol_version(params: Option<&serde_json::Value>) -> Option<ProtocolVersion> {
    let version = params?
        .get("_meta")?
        .get(meta::keys::PROTOCOL_VERSION)?
        .as_str()?;
    Some(ProtocolVersion::from_wire(version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::task::Poll;

    use serde_json::json;
    use tower::ServiceExt;
    use turbomcp_core::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

    #[derive(Clone)]
    struct Inner {
        fail: bool,
    }

    impl Service<JsonRpcMessage> for Inner {
        type Response = Option<JsonRpcMessage>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: JsonRpcMessage) -> Self::Future {
            let reply = match req {
                JsonRpcMessage::Request(r) if self.fail => Some(
                    JsonRpcResponse::error(
                        r.id,
                        JsonRpcError {
                            code: -32000,
                            message: "boom".into(),
                            data: None,
                        },
                    )
                    .into(),
                ),
                JsonRpcMessage::Request(r) => {
                    Some(JsonRpcResponse::success(r.id, json!({})).into())
                }
                _ => None,
            };
            std::future::ready(Ok(reply))
        }
    }

    #[tokio::test]
    async fn records_ok_and_error_without_panicking() {
        // Without a global meter provider installed, the instruments are no-ops;
        // the layer must still pass requests through cleanly (metrics are
        // best-effort observability, never load-bearing).
        let ok = MetricsLayer::new().layer(Inner { fail: false });
        let req: JsonRpcMessage = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } })),
        )
        .into();
        let resp = ok.oneshot(req).await.unwrap();
        assert!(matches!(resp, Some(JsonRpcMessage::Response(_))));

        let err = MetricsLayer::new().layer(Inner { fail: true });
        let req: JsonRpcMessage = JsonRpcRequest::new(2, "tools/call", None).into();
        let resp = err.oneshot(req).await.unwrap();
        let Some(JsonRpcMessage::Response(r)) = resp else {
            panic!("expected response")
        };
        assert!(r.error.is_some());
    }

    #[test]
    fn version_label_reads_meta() {
        let draft: JsonRpcMessage = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } })),
        )
        .into();
        assert_eq!(protocol_version_label(&draft), "2026-07-28");

        let bare: JsonRpcMessage = JsonRpcRequest::new(1, "ping", None).into();
        assert_eq!(protocol_version_label(&bare), "unknown");
    }

    #[test]
    fn composes_as_mcp_service() {
        fn assert_mcp_service<S: turbomcp_service::McpService>(_: &S) {}
        #[derive(Clone)]
        struct Dispatcher;
        impl Service<JsonRpcMessage> for Dispatcher {
            type Response = Option<JsonRpcMessage>;
            type Error = turbomcp_service::ProtocolError;
            type Future = std::future::Ready<Result<Self::Response, Self::Error>>;
            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
            fn call(&mut self, _: JsonRpcMessage) -> Self::Future {
                std::future::ready(Ok(None))
            }
        }
        assert_mcp_service(&MetricsLayer::new().layer(Dispatcher));
    }
}
