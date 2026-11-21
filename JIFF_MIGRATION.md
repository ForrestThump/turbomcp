# chrono -> jiff migration

This branch replaces every use of `chrono` in the TurboMCP workspace with
[`jiff`](https://docs.rs/jiff) (pinned to `=0.2.35`).

```toml
jiff = { version = "=0.2.35", features = ["serde"] }
```

The goal of the migration is twofold:

1. **`chrono` is gone from this repository's dependency graph.** Only two
   third-party crates still pull it in (see
   [Remaining transitive `chrono`](#remaining-transitive-chrono)).
2. **Nothing that crosses a wire changes.** Every timestamp that is
   serialized keeps the exact byte layout that `chrono::DateTime<Utc>`
   produced, so MCP peers, golden JSON fixtures and stored records stay
   compatible.

---

## Wire format: chrono-compatible RFC 3339

`chrono`'s serde implementation renders an RFC 3339 string in UTC with the `Z`
designator and a *fixed width* fractional second component chosen by chrono's
`SecondsFormat::AutoSi` rule:

| sub-second value | chrono output              | plain `jiff::Timestamp` output |
|------------------|----------------------------|--------------------------------|
| `0`              | `2026-11-20T22:15:03Z`     | `2026-11-20T22:15:03Z`         |
| `100_000_000`    | `2026-11-20T22:15:03.100Z` | `2026-11-20T22:15:03.1Z`       |
| `123_400_000`    | `2026-11-20T22:15:03.123400Z` | `2026-11-20T22:15:03.1234Z` |
| `123_456_789`    | `2026-11-20T22:15:03.123456789Z` | `2026-11-20T22:15:03.123456789Z` |

`jiff` prints the *shortest* fractional component that represents the value
exactly, so the two differ for values that are not a whole number of
milliseconds. To keep the wire format byte-for-byte identical, the migration
adds a small serde adapter in `turbomcp-core`:

```rust
// turbomcp-core/src/timestamp.rs
pub fn format(timestamp: &jiff::Timestamp) -> String;          // chrono serde layout ("Z")
pub fn format_with_offset(timestamp: &jiff::Timestamp) -> String; // chrono `to_rfc3339()` layout ("+00:00")
pub fn serialize<S>(timestamp: &jiff::Timestamp, serializer: S) -> ...;
pub fn deserialize<'de, D>(deserializer: D) -> ...;
pub mod option { /* the same for `Option<Timestamp>` */ }
```

It is used by:

* `turbomcp_protocol::types::Timestamp` (custom `Serialize`/`Deserialize`),
* `turbomcp_protocol::session::SessionEvent::timestamp`,
* `turbomcp_protocol::context::RequestInfo::timestamp`,
* `turbomcp_protocol::context::ClientSession::{connected_at,last_activity}`,
* `turbomcp_protocol::context::ConnectionMetrics::last_success`,
* `turbomcp_core::error::McpError::timestamp`,
* `turbomcp_transport::metrics::MetricsSnapshot::timestamp`.

Deserialization keeps accepting everything chrono accepted:

* `Z`, `+00:00` and any other numeric offset (`-05:00`, `+05:30`, ...),
* with or without a fractional second component,
* fractional seconds with more than nine digits are truncated (chrono silently
  dropped the extra digits, `jiff` rejects them).

The adapter is re-exported as `turbomcp_protocol::timestamp` so crates that
only depend on the protocol layer can use it too.

Note that `turbomcp_protocol::types::Timestamp` keeps two distinct
representations, exactly as it did with chrono:

* the **serialized** form uses the `Z` designator
  (`2026-11-20T22:15:03.123456Z`), which is what `chrono`'s serde
  implementation produced;
* the **`Display`** form uses chrono's `to_rfc3339()` shape, i.e. a numeric
  `+00:00` offset (`2026-11-20T22:15:03.123456+00:00`).

### Durations

`chrono::Duration` serialized as a two element array `[secs, nanos]` with the
fractional part kept unsigned (and in `0..1_000_000_000`) and the sign carried
by the seconds:

| duration      | chrono output            |
|---------------|--------------------------|
| 24 hours      | `[86400,0]`              |
| 1.5 seconds   | `[1,500000000]`          |
| -1.5 seconds  | `[-2,500000000]`         |

`jiff::SignedDuration` serializes as an ISO 8601 duration string (`"PT24H"`),
which is a different wire format. Durations therefore use the helpers in the
new `turbomcp_core::duration` module (re-exported as
`turbomcp_protocol::duration`), which write and read exactly chrono's shape:

```rust
#[serde(with = "turbomcp_core::duration")]
pub session_timeout: jiff::SignedDuration,
```

### JSON Schema

`schemars` 1.2 supports jiff natively, so the `chrono04` feature was replaced
by `jiff02`:

```toml
schemars = { version = "1.2.1", features = ["jiff02", "uuid1"] }
```

`jiff::Timestamp` schemas are emitted as `{"type": "string", "format":
"date-time"}`, i.e. exactly what `chrono::DateTime<Utc>` produced.

---

## Public API changes

Types move from chrono to jiff; the semantics stay the same.

### `turbomcp-protocol`

| Item | Before | After |
|------|--------|-------|
| `types::Timestamp` inner value | `chrono::DateTime<chrono::Utc>` | `jiff::Timestamp` |
| `types::Timestamp::from_datetime` | `fn(DateTime<Utc>) -> Timestamp` | `fn(jiff::Timestamp) -> Timestamp` |
| `types::Timestamp::datetime` | `-> DateTime<Utc>` | `-> jiff::Timestamp` |
| `types::Timestamp::elapsed` | `-> chrono::Duration` | `-> jiff::SignedDuration` |
| `impl From<DateTime<Utc>> for Timestamp` | — | `impl From<jiff::Timestamp> for Timestamp` |
| `context::ClientSession::connected_at` / `last_activity` | `DateTime<Utc>` | `jiff::Timestamp` |
| `context::ClientSession::session_duration` | `-> chrono::Duration` | `-> jiff::SignedDuration` |
| `context::ClientSession::is_idle` | `fn(chrono::Duration) -> bool` | `fn(jiff::SignedDuration) -> bool` |
| `context::RequestInfo::timestamp` | `DateTime<Utc>` | `jiff::Timestamp` |
| `context::ConnectionMetrics::last_success` | `Option<DateTime<Utc>>` | `Option<jiff::Timestamp>` |
| `session::SessionEvent::timestamp` | `DateTime<Utc>` | `jiff::Timestamp` |
| `session::SessionConfig::session_timeout` | `chrono::Duration` | `jiff::SignedDuration` |
| `session::SessionAnalytics::avg_session_duration` | `chrono::Duration` | `jiff::SignedDuration` |

New public item: `turbomcp_protocol::timestamp` (the chrono-compatible serde
helpers, re-exported from `turbomcp-core`).

Behaviour notes:

* `chrono::Duration::hours(24)` becomes `jiff::SignedDuration::from_hours(24)`;
  `chrono::Duration::zero()` becomes `jiff::SignedDuration::ZERO`.
* The `"date"` format validation no longer calls
  `chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")`. The replacement
  parses the components with `jiff::civil::Date` and keeps chrono's leniency
  (a one digit month/day such as `2025-1-7` is still accepted, out of range
  dates such as `2025-13-01` are still rejected).

### `turbomcp-core`

| Item | Before | After |
|------|--------|-------|
| `error::McpError::timestamp` (with `rich-errors`) | `chrono::DateTime<chrono::Utc>` | `jiff::Timestamp` |
| `error::McpError::timestamp()` (with `rich-errors`) | `-> chrono::DateTime<chrono::Utc>` | `-> jiff::Timestamp` |
| feature `rich-errors` | enabled `chrono` | enables `jiff` |

New public item: `turbomcp_core::timestamp` (the chrono-compatible serde
helpers). The module is compiled when the (implicit) `jiff` feature is
enabled, which `rich-errors` does.

### `turbomcp-transport`

| Item | Before | After |
|------|--------|-------|
| `metrics::MetricsSnapshot::timestamp` | `chrono::DateTime<chrono::Utc>` | `jiff::Timestamp` |
| `server::ServerTransportEvent::*::timestamp` | `chrono::DateTime<chrono::Utc>` | `jiff::Timestamp` |

### `turbomcp-websocket`

No public type changed. The `connected_at` / `disconnected_at` connection
metadata keeps the same RFC 3339 string; the value is parsed back into a
`jiff::Timestamp` instead of a `chrono::DateTime<Utc>`.

### `turbomcp-dpop`

No public type changed. The HSM backends label generated keys with
`jiff::Timestamp::now().as_second()`, which is the same whole Unix second
that `chrono::Utc::now().timestamp()` produced.

### `turbomcp-proxy`

No public type changed. The `generation_date` written into generated
projects is now jiff's RFC 3339 rendering (`Z` designator instead of
`+00:00`).

### `turbomcp-auth`, `turbomcp-client`

No public type changed and no runtime behaviour changed. Both crates
declared a `chrono` dependency that they never used (`turbomcp-auth` works
with `std::time::SystemTime`, `turbomcp-client` with `std::time::Instant`),
so the dependency was removed instead of being replaced. `turbomcp-auth` uses
`jiff` as a dev-dependency for the new expiry boundary tests.

### `turbomcp` (umbrella crate), `demo`, examples

No public type changed. The `current_time` tool of the examples returns
jiff's RFC 3339 rendering (`Z` designator) instead of chrono's
`to_rfc3339()` (`+00:00`).

---

## Semantics that were deliberately preserved

* **Integer second claims.** DPoP's `iat` and the auth layer's `exp`/`nbf`
  are whole Unix seconds. `jiff::Timestamp::now().as_second()` matches
  `chrono::Utc::now().timestamp()` for every current time. (For timestamps
  *before* the Unix epoch chrono floored while jiff truncates towards zero;
  nothing in this workspace timestamps the past, and the difference is pinned
  by a test.)
* **Expiry boundaries.** The auth layer compares `SystemTime::now()` against
  `UNIX_EPOCH + exp`, so a token whose `exp` is the current second is already
  expired. That boundary is unchanged and is now covered by tests
  (`crates/turbomcp-auth/tests/expiry_boundaries.rs`).
* **DPoP proof lifetime.** `DpopProof::is_expired` keeps its exact
  comparison, including the sub-second behaviour at the boundary
  (`crates/turbomcp-dpop/tests/expiry_boundaries.rs`).
* **Calendar and DST behaviour.** The protocol crate's civil date handling is
  covered by tests for the America/Chicago spring-forward gap
  (2026-03-08 02:30 does not exist), the fall-back ambiguity
  (2026-11-01 01:30 occurs twice), month-end arithmetic (Jan 31 + 1 month)
  and leap days (Feb 29) in
  `crates/turbomcp-protocol/tests/timestamp_compat.rs`.

---

## Remaining transitive `chrono`

`cargo tree -i chrono --workspace --all-features -e normal,build,dev --target all`
reports two remaining paths. Both are third-party crates that depend on
`chrono` unconditionally, so they cannot be removed without dropping those
crates:

```
chrono v0.4.44
├── oauth2 v5.0.0
│   └── turbomcp-auth            (OAuth 2.1 flows; `oauth2` requires chrono)
└── worker v0.8.1
    └── turbomcp-wasm            (optional `wasm-server`/`cloudflare` feature)
```

* `oauth2 5.0` has a mandatory (non-optional) `chrono` dependency.
* `worker 0.8.1` (Cloudflare Workers SDK) depends on `chrono`; it is only
  pulled in by the optional `wasm-server`/`cloudflare` feature of
  `turbomcp-wasm`.
* `serde_with 3.19` has an *optional* `chrono` dependency that no crate in
  this workspace enables, so it does not show up in the resolved graph.

Nothing in this repository uses `chrono` directly any more.
