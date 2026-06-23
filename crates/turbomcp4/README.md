# turbomcp4

A ground-up Rust SDK for the [Model Context Protocol](https://modelcontextprotocol.io),
both halves of the protocol — server **and** client — with a macro-driven,
zero-boilerplate surface and strict spec compliance as a feature.

> **Status: `4.0.0-alpha.1`.** This is the v4 rewrite, developed in parallel
> with the stable v3 crates (`turbomcp`). The crate is published under the
> `turbomcp4-*` names during the alpha and takes over the `turbomcp` name at GA.
> Edition 2024, MSRV 1.85.

## What you get

- **One macro defines a server.** `#[server]` over an `impl` block turns
  `#[tool]` / `#[resource]` / `#[prompt]` methods into a fully-wired MCP server.
  JSON schemas are generated from your function signatures at compile time, and
  the advertised capabilities are *derived* from which markers are present — they
  can't drift from the implementation.
- **Two protocol versions, one handler.** The same server answers both
  `2025-11-25` and the `DRAFT-2026-v1` draft. Your handlers speak
  version-neutral types; the version-specific wire shapes are conversions, not
  signature changes.
- **Transports behind one builder.** stdio (default) and Streamable HTTP
  (axum). `MyServer.run_stdio()` or `MyServer.into_server().run_http(addr, cfg)`.
- **The client too.** A typed `Client` runs the handshake, negotiates the
  version, and speaks the same neutral API — interoperating with the official
  Rust SDK (rmcp) both directions.
- **Production seams.** OAuth 2.1 resource-server auth, identity-keyed rate
  limiting, OpenTelemetry tracing, progress/logging, subscriptions, and
  bidirectional elicitation (MRTR) — each opt-in behind a feature flag.

## Quickstart

```rust
use turbomcp4::prelude::*;

#[derive(Clone)]
struct Hello;

#[server(name = "hello", version = "1.0.0")]
impl Hello {
    /// Say hello to someone.
    #[tool(description = "Say hello to someone")]
    async fn hello(&self, name: String) -> McpResult<String> {
        Ok(format!("Hello, {name}!"))
    }
}

#[tokio::main]
async fn main() -> Result<(), turbomcp4::ProtocolError> {
    // Logs MUST go to stderr — stdout carries the MCP protocol framing.
    Hello.run_stdio().await
}
```

Serve the same server over Streamable HTTP instead (feature `http`):

```rust,ignore
use turbomcp4::http::{HttpConfig, ServeHttp};

Hello.into_server()
    .run_http("127.0.0.1:8080".parse()?, HttpConfig::new())
    .await?;
```

## Tools, resources, prompts

```rust,ignore
#[server(name = "docs", version = "1.0.0")]
impl Docs {
    /// A tool: arguments come from the signature; the schema is generated.
    #[tool(description = "Add two numbers")]
    async fn add(&self, a: f64, b: f64) -> String { format!("{}", a + b) }

    /// A resource at a fixed URI (resources/list + resources/read).
    #[resource("config://app")]
    async fn config(&self) -> McpResult<String> { Ok(r#"{"debug":false}"#.into()) }

    /// A prompt template; its arguments are the function arguments.
    #[prompt]
    async fn summarize(&self, text: String) -> McpResult<String> {
        Ok(format!("Summarize:\n\n{text}"))
    }
}
```

Tools return `String`/`&str`, any numeric or `bool` scalar, `()` (empty
success), `Json<T>` (structured output — see below), or a
`neutral::CallToolResult` — each optionally wrapped in `McpResult<_>`. A
returned `McpError` becomes a tool-level error (`CallToolResult { isError }`) the
model can see — not a transport error.

### Structured output

Return `Json<T>` (where `T: Serialize + schemars::JsonSchema`) to produce a typed
result: the value goes in `structuredContent` with a JSON text mirror for
backward compatibility, and the macro generates the tool's `outputSchema` from
`T`.

```rust,ignore
#[derive(serde::Serialize, turbomcp4::schemars::JsonSchema)]
struct Stats { count: u64, mean: f64 }

#[tool(description = "Compute stats")]
async fn stats(&self) -> Json<Stats> { Json(Stats { count: 3, mean: 1.5 }) }
```

## Feature flags

| Feature | Enables |
|---|---|
| *(default)* | stdio transport (always linked) |
| `http` | Streamable HTTP transport (axum); the client's HTTP transport when `client` is on |
| `client` | the typed `Client` + `ConnectMode` negotiation |
| `auth` | OAuth 2.1 resource-server auth (bearer validation, RFC 9728 metadata) |
| `telemetry` | OpenTelemetry tracing (`TraceContextLayer`, W3C `_meta` propagation, PII-safe spans) |
| `ext-tasks` | the draft Tasks extension (`io.modelcontextprotocol/tasks`, SEP-2663) |

## Examples

In [`examples/`](examples/) — run with `cargo run -p turbomcp4 --example <name>`:

| Example | Shows |
|---|---|
| `hello_world` | the minimal one-tool server |
| `calculator` | several tools; infallible vs fallible returns |
| `stateful` | shared `Arc<RwLock<…>>` state across requests |
| `validation` | handler-body validation → tool-level errors |
| `resources_prompts` | the non-tool surface: resources + prompts |
| `structured_output` | `Json<T>` → `structuredContent` + generated `outputSchema` |
| `elicitation` | asking the user for input (MRTR + legacy inline) |
| `dual_transport` | one server over stdio **and** HTTP (`--features http`) |
| `tasks` | the draft Tasks extension (`--features ext-tasks`) |

## Migrating from v3

See [`MIGRATION.md`](MIGRATION.md) for the v3 → v4 deltas.

## License

MIT
