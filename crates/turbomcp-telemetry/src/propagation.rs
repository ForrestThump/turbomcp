//! W3C Trace Context propagation over MCP `_meta`.
//!
//! MCP carries distributed-tracing context in a request's `params._meta` (the
//! `traceparent`/`tracestate`/`baggage` keys, SEP-414) rather than HTTP headers,
//! so the same propagation works across stdio, HTTP, and WS. These adapters
//! bridge an MCP `_meta` object to the OpenTelemetry `Extractor`/`Injector`
//! interfaces and run the standard W3C propagators over it.

use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use serde_json::{Map, Value};

/// Adapts an MCP `_meta` object to the OTel [`Extractor`] interface (string
/// values only — the W3C keys are all strings).
struct MetaExtractor<'a>(&'a Map<String, Value>);

impl Extractor for MetaExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

/// Adapts an MCP `_meta` object to the OTel [`Injector`] interface.
struct MetaInjector<'a>(&'a mut Map<String, Value>);

impl Injector for MetaInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), Value::String(value));
    }
}

/// Extract a parent [`Context`] from an MCP `_meta` object — the trace context
/// (`traceparent`/`tracestate`) and baggage propagated by the caller. An empty
/// or trace-context-less `_meta` yields the default (root) context, so the
/// server starts a fresh trace.
#[must_use]
pub fn extract(meta: &Map<String, Value>) -> Context {
    let extractor = MetaExtractor(meta);
    // Run both standard propagators; baggage extends the trace-context result.
    let cx = TraceContextPropagator::new().extract(&extractor);
    BaggagePropagator::new().extract_with_context(&cx, &extractor)
}

/// Inject the trace context + baggage from `cx` into an MCP `_meta` object — for
/// a server acting as a client (outbound requests) to continue the trace.
pub fn inject(cx: &Context, meta: &mut Map<String, Value>) {
    let mut injector = MetaInjector(meta);
    TraceContextPropagator::new().inject_context(cx, &mut injector);
    BaggagePropagator::new().inject_context(cx, &mut injector);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TraceContextExt;
    use serde_json::json;

    #[test]
    fn extracts_traceparent_into_parent_context() {
        // A valid W3C traceparent: version-traceid-spanid-flags.
        let mut meta = Map::new();
        meta.insert(
            "traceparent".into(),
            json!("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
        );
        let cx = extract(&meta);
        let span = cx.span();
        let sc = span.span_context();
        assert!(sc.is_valid());
        assert_eq!(
            format!("{:032x}", sc.trace_id()),
            "0af7651916cd43dd8448eb211c80319c"
        );
    }

    #[test]
    fn empty_meta_yields_invalid_root_context() {
        let cx = extract(&Map::new());
        assert!(!cx.span().span_context().is_valid());
    }

    #[test]
    fn inject_then_extract_roundtrips_the_trace_id() {
        let mut meta = Map::new();
        meta.insert(
            "traceparent".into(),
            json!("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
        );
        let cx = extract(&meta);

        let mut out = Map::new();
        inject(&cx, &mut out);
        assert!(out.contains_key("traceparent"));
        // The injected context re-extracts to the same trace id.
        let again = extract(&out);
        assert_eq!(
            again.span().span_context().trace_id(),
            cx.span().span_context().trace_id(),
        );
    }
}
