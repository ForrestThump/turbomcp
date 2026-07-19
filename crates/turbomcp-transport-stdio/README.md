# turbomcp-transport-stdio

The TurboMCP v4 stdio transport: newline-delimited JSON-RPC over stdin/stdout (`LineTransport`, `serve_stdio`), with a bounded line reader as defense in depth.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
