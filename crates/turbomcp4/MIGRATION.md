# Migrating from v3 to v4

v4 is a ground-up rewrite. The macro surface — `#[server]`, `#[tool]`,
`#[resource]`, `#[prompt]` over a `Clone` struct — is intentionally
source-compatible for the common case, so simple servers port with little more
than an import change. The deltas below are the places where v4 deliberately
differs.

> This guide tracks the `4.0.0-alpha`. During the alpha the crate is published
> as `turbomcp4`; at GA it takes over the `turbomcp` name and the imports below
> change accordingly.

## Crate & imports

```rust
// v3
use turbomcp::prelude::*;

// v4 (alpha)
use turbomcp4::prelude::*;
```

The prelude still brings in the macros, `McpResult`, `McpError`, the per-RPC
context types, and `run_stdio`.

## Tool return types

v3 auto-stringified bare scalar returns (`-> i64`, `-> f64`, `-> bool`). v4 does
**not** — a tool returns `String`, `McpResult<String>`, or a
`neutral::CallToolResult`. Format the value yourself:

```rust
// v3
#[tool] async fn add(&self, a: i64, b: i64) -> i64 { a + b }

// v4
#[tool(description = "Add two numbers")]
async fn add(&self, a: f64, b: f64) -> String { format!("{}", a + b) }
```

This is explicit about exactly what text the client receives. A returned
`McpError` still becomes a tool-level error (`CallToolResult { isError: true }`),
not a transport error.

## Error constructors

`McpError::invalid_request` is gone. Use `invalid_params` for bad arguments —
the semantically correct JSON-RPC error for input validation:

```rust
// v3
return Err(McpError::invalid_request("age must be positive"));
// v4
return Err(McpError::invalid_params("age must be positive"));
```

Available constructors: `internal`, `invalid_params`, `method_not_found`,
`tool_not_found`, `tool_execution_failed`, `resource_not_found`,
`authentication`, `permission_denied`, `timeout`, `transport`.

## Contexts

v4 has a distinct context type per RPC (`CallToolContext`, `GetPromptContext`,
`ReadResourceContext`, `ListToolsContext`, …) instead of one shared
`RequestContext`. Add the relevant context as the first parameter when you need
it; omit it when you don't.

Bidirectional features (elicitation/sampling) live on `ctx.client` for the three
contexts that can carry them (`CallToolContext`, `GetPromptContext`,
`ReadResourceContext`):

```rust
#[tool(description = "Delete after confirmation")]
async fn delete(&self, ctx: &CallToolContext, path: String) -> McpResult<String> {
    let outcome = ctx.client.elicit("confirm", /* ElicitParams */).await?;
    // …
}
```

## Capabilities are derived, not declared

v3 had type-state capability builders. In v4, advertised capabilities are
*derived from the markers you write*: declaring a `#[resource]` is what
advertises the `resources` capability. There is no separate capabilities builder
to keep in sync — it cannot drift from the implementation.

## Two protocol versions, neutral handlers

One server answers both `2025-11-25` and the `DRAFT-2026-v1` draft. Handlers
speak version-neutral types (`turbomcp4::neutral`); the version-specific wire
shapes are conversions applied at the edges, not changes to your signatures.
Most servers never touch the wire types directly.

## Architecture changes (no drop-in equivalent yet)

These v3 constructs were replaced by different v4 mechanisms; code using them
needs rethinking rather than a mechanical port:

- **Middleware** — v3's `McpMiddleware` / `McpHandler` is replaced by
  `tower::Layer` composition over the `Service<JsonRpcMessage>` dispatcher (e.g.
  the telemetry `TraceContextLayer`). Cross-cutting concerns like auth and rate
  limiting are HTTP-transport-level seams (`HttpConfig::with_authenticator` /
  `with_rate_limiter`), not RPC middleware.
- **Composition** — `CompositeHandler` (mounting sub-servers under prefixes) has
  no v4 equivalent yet.
- **Visibility / progressive disclosure** — `VisibilityLayer` / `ComponentFilter`
  have no v4 equivalent yet.
- **Tags & versioning metadata** on components — not yet modeled in v4.

## Tasks

v3 surfaced Tasks one way. In v4 they split by protocol version:

- `2025-11-25`: Tasks are **core** — enable with `ServerBuilder::with_task_support()`.
- `DRAFT-2026-v1`: Tasks are an **extension** (`io.modelcontextprotocol/tasks`,
  SEP-2663) — enable the `ext-tasks` feature and register
  `TasksExtension` with `ServerBuilder::with_extension(...)`. The draft is
  session-less and server-directed (`resultType: "task"`, `tasks/get|update|cancel`,
  `notifications/tasks`).

## Not yet ported from v3

Tracked for later phases; absent in this alpha:

- **Transports**: TCP, Unix-socket, and WebSocket (v4 surfaces stdio + HTTP).
- **`#[completion]`** marker (the `WithCompletions` trait + dispatch exist; the
  macro marker does not).
- **Templated resource URIs** (RFC 6570, e.g. `file://{path}`) — v4 serves
  fixed-URI resources today.
