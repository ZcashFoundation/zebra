//! Benchmarks for batch verifiication of RedPallas signatures.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::{thread_rng, Rng};
use reddsa::{
    batch,
    orchard::{Binding, SpendAuth},
    Signature, SigningKey, VerificationKey, VerificationKeyBytes,
};

const MESSAGE_BYTES: &[u8; 0] = b"";

/// A batch verification item of a RedPallas signature variant.
///
/// This struct exists to allow batch processing to be decoupled from the
/// lifetime of the message. This is useful when using the batch verification
/// API in an async context. The different enum variants are for the different
/// signature types which use different Pallas basepoints for computation.
enum Item {
    SpendAuth {
        vk_bytes: VerificationKeyBytes<SpendAuth>,
        sig: Signature<SpendAuth>,
    },
    Binding {
        vk_bytes: VerificationKeyBytes<Binding>,
        sig: Signature<Binding>,
    },
}

/// Generates an iterator of random [Item]s
///
/// Each [Item] has a unique [SigningKey], randomly chosen [SigType] variant,
/// and signature over the empty message, "".
fn sigs_with_distinct_keys() -> impl Iterator<Item = Item> {
    std::iter::repeat_with(|| {
        let mut rng = thread_rng();
        // let msg = b"";
        match rng.gen::<u8>() % 2 {
            0 => {
                let sk = SigningKey::<SpendAuth>::new(thread_rng());
                let vk_bytes = VerificationKey::from(&sk).into();
                let sig = sk.sign(thread_rng(), &MESSAGE_BYTES[..]);
                Item::SpendAuth { vk_bytes, sig }
            }
            1 => {
                let sk = SigningKey::<Binding>::new(thread_rng());
                let vk_bytes = VerificationKey::from(&sk).into();
                let sig = sk.sign(thread_rng(), &MESSAGE_BYTES[..]);
                Item::Binding { vk_bytes, sig }
            }
            _ => panic!(),
        }
    })
}

/// Benchmarks batched vs unbatched RedPallas signature verification.
///
/// Includes heterogeneous groups across [SigType], [SigningKey]s, and messages.
fn bench_batch_verify(c: &mut Criterion) {
    let mut group = c.benchmark_group("Batch Verification");
    for &n in [8usize, 16, 24, 32, 40, 48, 56, 64].iter() {
        group.throughput(Throughput::Elements(n as u64));

        let sigs = sigs_with_distinct_keys().take(n).collect::<Vec<_>>();

        group.bench_with_input(
            BenchmarkId::new("Unbatched verification", n),
            &sigs,
            |b, sigs| {
                b.iter(|| {
                    for item in sigs.iter() {
                        match item {
                            Item::SpendAuth { vk_bytes, sig } => {
                                assert!(VerificationKey::try_from(*vk_bytes)
                                    .and_then(|vk| vk.verify(MESSAGE_BYTES, sig))
                                    .is_ok());
                            }
                            Item::Binding { vk_bytes, sig } => {
                                assert!(VerificationKey::try_from(*vk_bytes)
                                    .and_then(|vk| vk.verify(MESSAGE_BYTES, sig))
                                    .is_ok());
                            }
                        }
                    }
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("Batched verification", n),
            &sigs,
            |b, sigs| {
                b.iter(|| {
                    let mut batch = batch::Verifier::new();
                    for item in sigs.iter() {
                        match item {
                            Item::SpendAuth { vk_bytes, sig } => {
                                batch.queue(batch::Item::from_spendauth(
                                    *vk_bytes,
                                    *sig,
                                    MESSAGE_BYTES,
                                ));
                            }
                            Item::Binding { vk_bytes, sig } => {
                                batch.queue(batch::Item::from_binding(
                                    *vk_bytes,
                                    *sig,
                                    MESSAGE_BYTES,
                                ));
                            }
                        }
                    }
                    assert!(batch.verify(thread_rng()).is_ok())
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_batch_verify);
criterion_main!(benches);
