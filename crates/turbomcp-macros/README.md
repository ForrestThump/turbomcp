# turbomcp-macros

The TurboMCP v4 procedural macros: `#[server]`, `#[tool]`, `#[resource]`, `#[prompt]`, `#[completion]`, and `#[mcp_header]`. Schemas and capability advertisement are derived from your function signatures at compile time.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
