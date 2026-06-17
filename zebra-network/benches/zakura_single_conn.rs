//! Zakura frame codec micro-benchmarks.
#![allow(missing_docs)]

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use zebra_network::zakura::Frame;

fn frame_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("zakura_frame_codec");
    for size in [64usize, 1024, 16 * 1024] {
        let frame = Frame {
            message_type: 1,
            flags: 0,
            payload: vec![7; size],
        };
        let max = u32::try_from(size + 8).expect("benchmark payload fits u32");
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("encode_decode_{size}"), |b| {
            b.iter(|| {
                let encoded = frame.encode(max).expect("frame fits benchmark cap");
                let decoded = Frame::decode(black_box(&encoded), max).expect("frame decodes");
                black_box(decoded);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, frame_codec);
criterion_main!(benches);
