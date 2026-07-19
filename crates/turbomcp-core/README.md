# turbomcp-core

The `no_std + alloc` foundation of TurboMCP v4: `McpError`/`McpResult`, `ProtocolVersion`, the JSON-RPC message model, `Identity`, `RequestContext`, and the `_meta` handling (propagation, sanitization, internal keys). wasm32-portable.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
