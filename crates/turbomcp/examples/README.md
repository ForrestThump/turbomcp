# TurboMCP Examples

Sixteen focused examples demonstrate the current TurboMCP 3.1 API, from a
single-tool STDIO server to transport pairs, in-memory testing, composition,
visibility filtering, and middleware.

## Quick Start

```bash
# Simplest STDIO server
cargo run -p turbomcp --example hello_world

# Macro-based server with several tools
cargo run -p turbomcp --example macro_server

# In-memory test client, no network transport
cargo run -p turbomcp --example test_client

# Metadata-only examples that print and exit
cargo run -p turbomcp --example visibility
cargo run -p turbomcp --example composition
cargo run -p turbomcp --example middleware
```

STDIO server examples reserve stdout for JSON-RPC. Use an MCP client, or send a
complete MCP session over stdin:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"smoke","version":"1.0"},"capabilities":{}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hello","arguments":{"name":"World"}}}' \
  | cargo run -p turbomcp --example hello_world
```

## Server Examples

| Example | What It Teaches |
| --- | --- |
| `hello_world.rs` | Minimal one-tool STDIO server |
| `macro_server.rs` | `#[turbomcp::server]` and `#[tool]` macro patterns |
| `calculator.rs` | Basic arithmetic tools with structured numeric inputs |
| `stateful.rs` | Shared state with `Arc<RwLock<T>>` |
| `validation.rs` | Hand-written parameter validation inside tool handlers |
| `tags_versioning.rs` | Tool/resource/prompt tags and version metadata |

`tags_versioning.rs` prints metadata by default. Start its STDIO server only
when you explicitly pass `--serve`:

```bash
cargo run -p turbomcp --example tags_versioning
cargo run -p turbomcp --example tags_versioning -- --serve
```

## Advanced Patterns

| Example | What It Teaches |
| --- | --- |
| `visibility.rs` | `VisibilityLayer` tag filtering and session-specific access |
| `composition.rs` | `CompositeHandler` with prefixed mounted handlers |
| `middleware.rs` | Typed middleware for logging, metrics, and access control |
| `test_client.rs` | `McpTestClient` assertions without a transport |
| `type_state_builders_demo.rs` | Type-state builders for compile-time setup guarantees |

## Transport Examples

| Example | Transport | What It Teaches |
| --- | --- | --- |
| `tcp_server.rs` | TCP | Network server with `run_tcp()` |
| `tcp_client.rs` | TCP | TCP client using `turbomcp-client` |
| `unix_server.rs` | Unix socket | Same-host IPC server with `run_unix()` |
| `unix_client.rs` | Unix socket | Unix socket client using `turbomcp-client` |
| `transports_demo.rs` | STDIO plus feature-gated transports | Explicit `run_*` transport methods enabled by Cargo features |

Run TCP in two terminals:

```bash
cargo run -p turbomcp --example tcp_server --features tcp
cargo run -p turbomcp --example tcp_client --features "tcp full-client"
```

Run Unix sockets on Unix/Linux/macOS in two terminals:

```bash
cargo run -p turbomcp --example unix_server --features unix
cargo run -p turbomcp --example unix_client --features "unix full-client"
```

Inspect transport method availability:

```bash
cargo run -p turbomcp --example transports_demo --features "http tcp"
```

## Feature Notes

Most STDIO examples only need the default `stdio` feature:

```bash
cargo run -p turbomcp --example calculator
cargo run -p turbomcp --example stateful
```

Transport clients require `full-client` in addition to the transport feature
because they use `turbomcp-client`.

Compile every `turbomcp` example with all features:

```bash
cargo check -p turbomcp --examples --all-features
```

## Related Documentation

- [TurboMCP Documentation](https://docs.rs/turbomcp)
- [MCP Specification](https://modelcontextprotocol.io)
- [Migration Guide](../../../MIGRATION.md)
- [Main README](../../../README.md)
- [OpenAPI Integration](../../turbomcp-openapi/README.md)
