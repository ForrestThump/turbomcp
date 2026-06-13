//! Benchmarks for `MessageBatch` construction.
//!
//! `MessageBatch::add` accumulates messages into one contiguous buffer. The
//! cost under measurement is how that buffer grows across N successive adds:
//! a per-add full copy of the accumulator is O(n) per call and O(n²) over the
//! batch, while an amortized append is O(1) per call and O(n) over the batch.
//!
//! The sweep over N = 10/100/1000 is what exposes the difference: if per-add
//! cost is constant, total time grows ~linearly with N; if per-add cost grows
//! with the accumulated size, total time grows ~quadratically. Throughput is
//! reported in elements so the per-message cost is directly comparable across
//! N.
//!
//! Run with: `cargo bench -p turbomcp-protocol --bench message_batch`

use std::hint::black_box;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use turbomcp_protocol::zero_copy::{MessageBatch, MessageId};

/// Payload size per message — representative of a small/medium JSON-RPC frame.
const PAYLOAD_BYTES: usize = 256;

/// Pre-build N `(id, payload)` pairs so the timed section measures only the
/// batch building, not input construction.
fn inputs(n: usize) -> Vec<(MessageId, Bytes)> {
    (0..n)
        .map(|i| {
            let payload = Bytes::from(vec![b'x'; PAYLOAD_BYTES]);
            (MessageId::from(format!("msg-{i}")), payload)
        })
        .collect()
}

fn bench_batch_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_batch_creation");

    for n in [10usize, 100, 1000] {
        let pairs = inputs(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &pairs, |b, pairs| {
            b.iter_batched(
                || pairs.clone(),
                |pairs| {
                    let mut batch = MessageBatch::new(pairs.len());
                    for (id, payload) in pairs {
                        batch.add(id, payload);
                    }
                    black_box(batch)
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_batch_creation);
criterion_main!(benches);
