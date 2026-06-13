# Perf PR 2 — `MessageBatch` O(n²) construction

**Date:** 2026-06-12
**Branch:** `perf/pr2-message-batch-quadratic`
**Scope:** roadmap item 2a (`perf_roadmap.md`, "PR 2 — O(n²) message batch building")

## Summary

`MessageBatch::add` recopied the entire accumulated buffer on every call,
making batch construction O(n²) in the number of messages. The fix reclaims
the accumulator in place so each payload is copied exactly once — O(n) over the
batch — with no public API change.

| N (messages, 256 B each) | Before | After | Δ time |
|---|---|---|---|
| 10 | 1.94 µs | 1.18 µs | −40 % |
| 100 | 76.1 µs | 9.30 µs | −88 % |
| 1000 | 4.81 ms | 83.6 µs | **−98.3 % (~57×)** |

The decisive evidence is per-message throughput: under the old code it
collapsed as the batch grew (5.15 → 1.31 → 0.21 Melem/s), the signature of a
per-add cost that scales with accumulated size. After the fix it is flat at
~8–12 Melem/s regardless of N — the per-add cost no longer depends on how much
has already been added.

## The bug

[`crates/turbomcp-protocol/src/zero_copy.rs`](../../crates/turbomcp-protocol/src/zero_copy.rs), `MessageBatch::add`, before:

```rust
let mut buffer = BytesMut::from(self.buffer.as_ref()); // copies the whole prefix
buffer.extend_from_slice(&payload);
self.buffer = buffer.freeze();
```

`self.buffer` is the contiguous accumulator of every message added so far.
`BytesMut::from(self.buffer.as_ref())` allocates a fresh buffer and copies the
**entire** accumulated content, every call, before appending the new payload.
Adding message *k* copies *k−1* payloads' worth of bytes, so building a batch
of *n* messages copies ~n²/2 payloads total.

## The fix

```rust
let mut buffer = std::mem::take(&mut self.buffer)
    .try_into_mut()
    .unwrap_or_else(|shared| BytesMut::from(shared.as_ref()));
buffer.extend_from_slice(&payload);
self.buffer = buffer.freeze();
```

`Bytes::try_into_mut` reclaims the existing allocation as a `BytesMut` in O(1)
**when the `Bytes` is uniquely owned**, preserving its spare capacity. The
accumulator is uniquely owned in the normal case: `get`/`iter` hand out
short-lived `Bytes` slices that are dropped before the next `add`. So the
append path becomes `extend_from_slice` into a buffer that grows geometrically
(like `Vec`) — each payload copied once, O(n) over the batch, with `freeze()`
itself O(1).

If a caller *does* hold a slice across an `add` (the buffer is then shared and
can't be reclaimed in place), `try_into_mut` returns the `Bytes` back and the
`unwrap_or_else` arm falls back to the old copy. This keeps the result correct
in every case and only pays the old cost in that rare scenario.

**Why no API change:** `buffer`, `messages`, and `ids` stay `pub` with the same
types; `new`/`add`/`get`/`iter` keep their signatures. A workspace-wide search
found no code outside the type's own `impl` and tests that reads the `buffer`
field, so the internal representation was free to change. `try_into_mut` is
available in `bytes` 1.11 (the pinned version).

## Methodology

* **Hardware/OS:** AMD Ryzen 5 7500X3D (6C/12T), Windows 10 Pro 19045.
* **Toolchain:** rustc 1.94.1, criterion 0.8.2.
* **Settings:** 100 samples, 1 s warm-up, 3 s measurement, `--noplot`. criterion
  reported `p = 0.00 < 0.05` for every comparison.
* **Bench:** [`crates/turbomcp-protocol/benches/message_batch.rs`](../../crates/turbomcp-protocol/benches/message_batch.rs) —
  builds a batch of N messages (256 B each) from pre-constructed inputs
  (`iter_batched` setup, so only the `add` loop is timed), swept over
  N = 10/100/1000 with `Throughput::Elements(N)` so per-message cost is
  comparable across N.

Reproduction:

```sh
git checkout <PR2 bench scaffolding commit>   # bench exists, add() still quadratic
cargo bench -p turbomcp-protocol --bench message_batch -- --save-baseline before
git checkout perf/pr2-message-batch-quadratic
cargo bench -p turbomcp-protocol --bench message_batch -- --baseline before
```

### Full numbers

| N | Before (total) | Before (per-elem thrpt) | After (total) | After (per-elem thrpt) |
|---|---|---|---|---|
| 10 | 1.94 µs | 5.15 Melem/s | 1.18 µs | 8.44 Melem/s |
| 100 | 76.1 µs | 1.31 Melem/s | 9.30 µs | 10.75 Melem/s |
| 1000 | 4.81 ms | 0.21 Melem/s | 83.6 µs | 11.97 Melem/s |

Scaling check (total time per 10× increase in N): before ×39 then ×63
(super-linear → quadratic); after ×7.9 then ×9.0 (linear).

## Correctness evidence

* `cargo test -p turbomcp-protocol` — 383 tests pass (249 lib + integration
  suites), zero failures.
* New `test_message_batch_byte_equality_many`: builds a 500-message batch with
  distinct, varying-length payloads and asserts every message round-trips
  byte-for-byte via both `get` (random access) and `iter` (sequential), plus
  offset/length contiguity and that the final offset equals `buffer.len()`.
* New `test_message_batch_shared_buffer_fallback`: holds a `get(0)` slice across
  a subsequent `add`, forcing the shared-buffer fallback path, and asserts both
  the held slice and the batch remain correct.
* Pre-existing `test_message_batch` (3-message get/iter/byte-equality) passes
  unmodified.
* `cargo clippy -p turbomcp-protocol --benches --tests` — clean.

## Note on the roadmap's bench assumption

The roadmap listed this as the one PR with a pre-existing baseline, pointing at
`message_batch_creation` in `benches/regression/performance_regression_detector.rs`,
and flagged "verify it adds multiple messages" as the assumption to check first.

The check turned up something more fundamental: that file lives in a root
`benches/` directory that is **not a workspace member** and has no `Cargo.toml`,
so it is not a registered bench target and cannot be built or run in this
workspace (`cargo bench --bench regression_detector` → "no bench target named
`regression_detector`"). There was no usable pre-existing baseline. This PR adds
a real, registered bench in `turbomcp-protocol` (the crate that owns
`MessageBatch`) instead. The orphaned file was left untouched to avoid
scope creep.
