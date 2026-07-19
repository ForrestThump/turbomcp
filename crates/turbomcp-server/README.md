# turbomcp-server

The TurboMCP v4 server: `VersionDispatcher` (both protocol versions, one handler surface), capability traits (`WithTools`/`WithResources`/`WithPrompts`/`WithCompletions`), `ServerBuilder`, sessions, core Tasks, subscriptions, progress/logging, MRTR client interaction, and the SEP-2549 cache policy.

Part of [TurboMCP](https://github.com/Epistates/turbomcp), a Rust SDK for the
[Model Context Protocol](https://modelcontextprotocol.io). Most users should
depend on the [`turbomcp`](https://crates.io/crates/turbomcp) facade, which
re-exports this crate's surface behind one dependency and its feature flags.

## License

MIT
