# Perf PR 5 — Unbounded collections / backpressure

**Date:** 2026-06-12
**Branch:** `perf/pr5-bounded-collections` (stacked on `perf/pr1-transport-io-overhead`)
**Scope:** roadmap items 2d, 2j (`perf_roadmap.md`, "PR 5 — Unbounded collections / backpressure")

## Summary

This is a **correctness/safety PR, not a speed PR**. Two collections that could
grow without bound under bursty or adversarial load are now capped. The
benchmark evidence proves *no throughput regression*, not *faster*.

| Item | Crate | Change | Safety win |
|------|-------|--------|-----------|
| 2d | `turbomcp-server` | Cap SSE subscribers per session at 16, prune dead senders on subscribe | Reconnect storm can no longer grow the subscriber vector without bound |
| 2j | `turbomcp-stdio`, `turbomcp-http` | Inbound message channel capacity 1000 → 32 | Bounded in-flight memory; real backpressure instead of buffering up to 1000 messages |

## Why this is stacked on PR 1

PR 5 reuses PR 1's transport benchmarks (`sse_throughput`, `line_parse`) as a
regression guard, and those benches depend on PR 1's `#[doc(hidden)] pub`
visibility hooks — they do not compile against `main`. So this branch is built
on top of `perf/pr1-transport-io-overhead`; its GitHub base should be that
branch, and it should be rebased onto `main` once PR 1 merges.

## 2d — Bounded SSE subscribers

### The bug

[`crates/turbomcp-server/src/transport/http.rs`](../../crates/turbomcp-server/src/transport/http.rs):
`SessionData.subscribers: Vec<UnboundedSender<…>>` had no cap. Dead senders
(from SSE clients that disconnected) were only removed lazily, when a message
happened to be routed through `send_to_session`/`broadcast`. A client that
repeatedly opens a `GET /sse` stream and drops it — a reconnect storm, whether
from a flaky network or a malicious peer — appends a sender each time and
nothing prunes them until a send occurs. The vector grows without bound.

### The fix

`subscribe_session` now (a) prunes closed senders first, then (b) enforces
`MAX_SUBSCRIBERS_PER_SESSION = 16`:

```rust
data.subscribers.retain(|tx| !tx.is_closed());
if data.subscribers.len() >= MAX_SUBSCRIBERS_PER_SESSION {
    return Err(SubscribeError::AtCapacity);
}
```

Pruning before the check means the cap counts *live* subscribers — a
disconnected client immediately frees its slot, so legitimate reconnection
never trips the limit, while a storm of dead connections is reclaimed rather
than accumulated. The return type changed from `Option<Receiver>` to
`Result<Receiver, SubscribeError>` so the GET handler can distinguish the two
failure modes and answer correctly:

* `SessionNotFound` → `404 Not Found` (unchanged behavior for unknown sessions)
* `AtCapacity` → `429 Too Many Requests` (sheds the excess connection)

16 is generous: the MCP spec permits multiple streams per session for
resumability overlap, but a sane client uses one or two. The cap exists to
bound the pathological case, not to constrain normal use.

### Why no consequential API change

`subscribe_session` is `pub` but is plumbing for the crate's own SSE handler;
a workspace search found no external caller. The `Option → Result` change is
source-compatible with the existing `.expect(...)` call sites (bench + unit
test) because `SubscribeError: Debug`. The only caller that inspects the
variants is the GET handler, updated here.

## 2j — Bounded message channels

[`crates/turbomcp-stdio/src/transport.rs`](../../crates/turbomcp-stdio/src/transport.rs)
and [`crates/turbomcp-http/src/transport.rs`](../../crates/turbomcp-http/src/transport.rs)
created their inbound channels with `mpsc::channel(1000)`. Both producers (the
stdio reader task, the HTTP SSE task) already push with `send().await`, so the
oversized buffer bought nothing: it just allowed up to 1000 potentially-large
messages to accumulate in memory before backpressure engaged. Lowered to a
named `…CHANNEL_CAPACITY = 32`. These are single-peer (stdio) / single-stream
(SSE) paths, so the capacity is a memory-safety knob, not a throughput one —
backpressure now parks the producer ~32 messages deep instead of ~1000.

The HTTP client's *response* channel (`mpsc::channel(100)`, used for
request/response correlation) was left unchanged, matching the roadmap's
narrower scope.

## Correctness evidence

Tests added (all passing; `cargo clippy` clean across the three crates):

* `subscribe_session_enforces_capacity` (server) — 16 subscribers succeed while
  their receivers are held; the 17th returns `Err(AtCapacity)`.
* `subscribe_session_reclaims_capacity_after_disconnect` (server) — fill to cap,
  reject the next, drop all receivers, then a subsequent subscribe succeeds.
  This is the reconnect-storm safety property: dead subscribers free their slots.
* `subscribe_session_unknown_session_is_not_found` (server) — `Err(SessionNotFound)`.
* `bounded_receive_channel_delivers_in_order_under_backpressure` (stdio) — a
  producer writes 200 messages (well over both the 256-byte pipe and the
  32-slot channel) from a spawned task while the consumer drains; asserts all
  200 arrive **in order, with no loss and no deadlock**. This exercises the
  full end-to-end backpressure chain with the lowered capacity.

Full suites: `turbomcp-server --features http` (155 tests), `turbomcp-stdio`
(24), `turbomcp-http` (9) — all green.

## Regression evidence (and its limits)

PR 1's benches were re-run on PR 5 against a baseline captured on the PR 1
branch (criterion, AMD Ryzen 5 7500X3D, rustc 1.94.1; 100 samples, 1 s warm-up,
3 s measurement).

| Bench (timed path) | PR 1 | PR 5 | Δ |
|---|---|---|---|
| send_to_session/64 | 143 ns | 147 ns | +2.8 % |
| send_to_session/4096 | 178 ns | 184 ns | +2.8 % |
| send_to_session/65536 | 1.40 µs | 1.32 µs | −5.7 % |
| broadcast/1 | 138 ns | 137 ns | −0.7 % |
| broadcast/8 | 298 ns | 290 ns | −2.7 % |
| broadcast/64 | 1.95 µs | 1.91 µs | −2.1 % |
| stdio_parse/128 | 1.06 µs | 1.14 µs | +7.5 % |
| stdio_parse/8192–65536 | — | — | improved |

All deltas are run-to-run noise: they are sub-10 %, have no consistent
direction, and the timed code paths (`send_to_session`, `broadcast`,
`sse_event_bytes`, `parse_message`) are **byte-identical** between PR 1 and
PR 5. criterion's default ±5 % threshold labels several as "regressed"/
"improved," but that is sampling noise across two separate builds, not real
change. Conclusion: **no throughput regression.**

**Honest caveat on coverage.** These benches bound *collateral* regression —
they confirm PR 5's `subscribe_session` change (cap + prune, which runs in the
bench's untimed setup) and the channel-capacity edits did not slow the SSE send
path or line parsing. They do **not** directly measure the bounded channels
changed in 2j: `line_parse` exercises parsing only, and `sse_throughput`
measures the server's *subscriber* channels, which are `UnboundedSender` and
unaffected by the 2j capacity change. The bounded channels live on the stdio
receive path and the HTTP client SSE path, which have no microbenchmark. The
direct evidence for 2j is therefore the backpressure delivery test above
(no loss/reorder/deadlock at capacity 32) plus the reasoning that, for a
single-peer transport with `send().await`, buffer size trades memory for
nothing on throughput.
