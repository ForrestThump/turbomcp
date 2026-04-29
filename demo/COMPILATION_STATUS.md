# Demo Compilation Status

## Current Status: Verified

The demo is a workspace member and builds with the current TurboMCP API.

Verified commands:

```bash
cargo check -p turbomcp-demo
cargo test -p turbomcp-demo --all-targets
```

The server exposes three tools over STDIO:

- `hello`
- `add`
- `current_time`

The demo intentionally stays minimal; broader production patterns live in
`crates/turbomcp/examples` and the crate-level examples for auth, proxy, and
OpenAPI.
