# turbomcp-codec

The TurboMCP v4 wire codec: bytes ↔ `JsonRpcMessage`. `serde_json` is the always-available portable baseline; the `simd` feature swaps in sonic-rs on native x86_64/aarch64.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
