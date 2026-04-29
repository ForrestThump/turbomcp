# turbomcp-proxy Examples

This directory contains practical examples for proxy construction, backend
introspection, and schema export.

## Available Examples

### `runtime_proxy.rs` - Runtime Proxy Builder

```bash
cargo run -p turbomcp-proxy --example runtime_proxy
```

Demonstrates `RuntimeProxyBuilder`, backend/frontend configuration, security
validation, and proxy metrics. Some attempted backends are intentionally invalid
so the example can show validation errors without requiring external services.

### `tcp_backend.rs` - TCP Backend Introspection

```bash
# Terminal 1: start the workspace TCP MCP server
cargo run -p turbomcp --example tcp_server --features tcp

# Terminal 2: connect and introspect it
cargo run -p turbomcp-proxy --example tcp_backend
```

Shows how to configure a TCP backend, initialize the MCP client connection, and
print tools/resources/prompts discovered from the backend.

### `unix_socket_backend.rs` - Unix Socket Backend Introspection

```bash
# Terminal 1: start the workspace Unix socket MCP server
cargo run -p turbomcp --example unix_server --features unix

# Terminal 2: connect and introspect it
cargo run -p turbomcp-proxy --example unix_socket_backend
```

Shows the same introspection path using a Unix domain socket. This example is
for Unix/Linux/macOS and requires the socket file to exist.

### `schema_export.rs` - Schema Generation

```bash
# Self-contained mock spec; no backend required
cargo run -p turbomcp-proxy --example schema_export

# Real STDIO backend
cargo run -p turbomcp-proxy --example schema_export -- \
  --backend stdio --cmd "your-mcp-server"

# Real TCP backend
cargo run -p turbomcp-proxy --example schema_export -- \
  --backend tcp --tcp 127.0.0.1:8765

# Real Unix socket backend
cargo run -p turbomcp-proxy --example schema_export -- \
  --backend unix --unix /tmp/turbomcp-demo.sock
```

Generates OpenAPI 3.1, GraphQL SDL, and Protobuf 3 definitions from an MCP
server capability snapshot. With no arguments it uses a built-in mock spec so
the example is always runnable.

## CLI Equivalents

```bash
turbomcp-proxy serve \
  --backend tcp --tcp 127.0.0.1:8765 \
  --frontend http --bind 127.0.0.1:3001

turbomcp-proxy serve \
  --backend unix --unix /tmp/turbomcp-demo.sock \
  --frontend http --bind 127.0.0.1:3002

turbomcp-proxy schema openapi \
  --backend tcp --tcp 127.0.0.1:8765 \
  --output api-spec.json

turbomcp-proxy schema protobuf \
  --backend stdio --cmd "your-mcp-server" \
  --output server.proto
```

## Requirements

- Rust 1.89.0 or newer
- A live MCP backend for `tcp_backend.rs` and `unix_socket_backend.rs`
- No external backend for `runtime_proxy.rs` or mock-mode `schema_export.rs`

## Troubleshooting

`Connection refused`: ensure the TCP backend is running on the configured host
and port.

`Socket not found`: ensure the Unix socket path exists and the server has
created it before running the example.

`Backend connection error`: verify the backend speaks MCP over the selected
transport and can complete initialization.
