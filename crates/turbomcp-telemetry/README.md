# turbomcp-telemetry

TurboMCP v4 observability: OpenTelemetry traces (`TraceContextLayer`, W3C context via MCP `_meta`, PII-safe identity) and metrics (`MetricsLayer`: request count, duration, in-flight), with turnkey OTLP export behind the `otlp` feature.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
