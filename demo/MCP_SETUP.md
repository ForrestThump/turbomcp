# TurboMCP Demo - MCP Server Setup

The demo is a minimal STDIO MCP server with three tools: `hello`, `add`, and
`current_time`.

## Build

```bash
cargo build -p turbomcp-demo --release
```

## Claude Desktop or LM Studio

Use the compiled binary for the most predictable startup behavior:

```json
{
  "mcpServers": {
    "turbomcp-demo": {
      "command": "/absolute/path/to/turbomcp/target/release/turbomcp-demo",
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

For local development, running through Cargo is also valid:

```json
{
  "mcpServers": {
    "turbomcp-demo": {
      "command": "cargo",
      "args": [
        "run",
        "-p",
        "turbomcp-demo"
      ],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

## Tool Examples

```json
{
  "name": "hello",
  "arguments": {
    "name": "TurboMCP"
  }
}
```

```json
{
  "name": "add",
  "arguments": {
    "a": 2,
    "b": 3
  }
}
```

```json
{
  "name": "current_time",
  "arguments": {}
}
```

## Troubleshooting

- Build from the workspace root so Cargo uses the workspace lockfile.
- Prefer the release binary in desktop client configs to avoid compile output
  during client startup.
- Keep all human-readable logging on stderr; stdout is reserved for MCP
  JSON-RPC frames.
