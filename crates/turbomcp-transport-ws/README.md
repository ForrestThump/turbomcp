# turbomcp-transport-ws

The TurboMCP v4 WebSocket transport (bidirectional; not part of the MCP spec): `serve_websocket` + `connect`, with upgrade-time Origin policy, bearer auth, message-size caps, and idle-peer reaping.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
