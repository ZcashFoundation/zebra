//! The real `tower-batch-control` worker around a harness-owned RedJubjub
//! service. As in the original oracle, RNG is seeded, items accumulate until
//! flush, and flush verifies synchronously. This is not Zebra's private
//! verifier service or its fallback. Findings must also exclude harness bugs.

use std::{
    future::Future,
    mem,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
    time::Duration,
};

use bls12_381::Bls12;
use futures::future::join_all;
use groth16::Proof;
use group::GroupEncoding;
use sapling_crypto::value::CommitmentSum;
use tokio::{runtime::Runtime, sync::watch};
use tower::{Service, ServiceExt};
use tower_batch_control::{Batch, BatchControl, RequestWeight};
use zebra_chain::primitives::redjubjub as rj;

use super::{assert_no_false_accept, rng, seed, sighash, transactions, MAX_ITEMS};

#[derive(Clone)]
pub struct Item(pub rj::batch::Item);
impl RequestWeight for Item {}

/// Reproduce the signature enqueue prefix of Sapling's check_bundle, including
/// decoding/small-order checks before each enqueue. Groth16 proofs are decoded,
/// not verified: the property here is signature batching and worker partitioning.
pub fn items(stream: &[u8]) -> Vec<Item> {
    let mut items = Vec::new();
    for tx in transactions(stream) {
        let Some(hash) = sighash(&tx) else { continue };
        let Some(bundle) = tx.sapling_bundle() else {
            continue;
        };
        let mut cv = CommitmentSum::zero();
        let mut complete = true;
        for spend in bundle.shielded_spends() {
            if items.len() == MAX_ITEMS {
                return items;
            }
            let rk: Option<jubjub::AffinePoint> =
                jubjub::AffinePoint::from_bytes((*spend.rk()).into()).into();
            if Proof::<Bls12>::read(&spend.zkproof()[..]).is_err()
                || rk.is_none_or(|rk| bool::from(rk.is_small_order()))
            {
                complete = false;
                break;
            }
            cv += spend.cv();
            items.push(Item(
                (
                    rj::VerificationKeyBytes::<rj::SpendAuth>::from(*spend.rk()),
                    *spend.spend_auth_sig(),
                    &hash.0,
                )
                    .into(),
            ));
        }
        if !complete {
            continue;
        }
        for output in bundle.shielded_outputs() {
            let epk: Option<jubjub::ExtendedPoint> =
                jubjub::ExtendedPoint::from_bytes(&output.ephemeral_key().0).into();
            if epk.is_none_or(|epk| bool::from(epk.is_small_order()))
                || Proof::<Bls12>::read(&output.zkproof()[..]).is_err()
            {
                complete = false;
                break;
            }
            cv -= output.cv();
        }
        if complete {
            if items.len() == MAX_ITEMS {
                return items;
            }
            items.push(Item(
                (
                    rj::VerificationKeyBytes::<rj::Binding>::from(
                        cv.into_bvk(*bundle.value_balance()),
                    ),
                    bundle.authorization().binding_sig,
                    &hash.0,
                )
                    .into(),
            ));
        }
    }
    items
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BatchLog = Arc<Mutex<Vec<usize>>>;

struct Verifier {
    items: Vec<Item>,
    sender: watch::Sender<Option<bool>>,
    seed: u64,
    log: BatchLog,
}

impl Verifier {
    fn flush(&mut self) {
        let items = mem::take(&mut self.items);
        let (new_sender, _) = watch::channel(None);
        let sender = mem::replace(&mut self.sender, new_sender);
        if items.is_empty() {
            return;
        }
        self.log
            .lock()
            .expect("batch log is not poisoned")
            .push(items.len());
        let mut verifier = rj::batch::Verifier::new();
        for item in items {
            verifier.queue(item.0);
        }
        let _ = sender.send(Some(verifier.verify(rng(self.seed)).is_ok()));
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        self.flush();
    }
}

impl Service<BatchControl<Item>> for Verifier {
    type Response = bool;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<bool, BoxError>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: BatchControl<Item>) -> Self::Future {
        match request {
            BatchControl::Item(item) => {
                self.items.push(item);
                let mut receiver = self.sender.subscribe();
                Box::pin(async move {
                    receiver
                        .changed()
                        .await
                        .map_err(|_| "batch dropped without verdict")?;
                    let verdict = *receiver.borrow();
                    verdict.ok_or_else(|| "flush produced no verdict".into())
                })
            }
            BatchControl::Flush => {
                self.flush();
                Box::pin(async { Ok(true) })
            }
        }
    }
}

pub fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fuzz runtime can be constructed")
    })
}

async fn verdicts(items: &[Item], width: usize, seed: u64) -> Vec<bool> {
    let log = BatchLog::default();
    let (sender, _) = watch::channel(None);
    let verifier = Verifier {
        items: Vec::new(),
        sender,
        seed,
        log: log.clone(),
    };
    let mut service = Batch::new(verifier, width, None, Duration::from_secs(30));
    let mut futures = Vec::new();
    for item in items {
        futures.push(
            service
                .ready()
                .await
                .expect("worker accepts item")
                .call(item.clone()),
        );
    }
    let verdicts: Vec<_> = join_all(futures)
        .await
        .into_iter()
        .map(|result| result.expect("every item receives a verdict"))
        .collect();
    // Verify the actual scheduler partition, not merely the requested capacity.
    assert_eq!(
        *log.lock().expect("batch log is not poisoned"),
        vec![width; items.len() / width]
    );
    verdicts
}

pub fn partition(count: usize, control: u8) -> Option<(usize, usize, usize)> {
    if count < 2 {
        return None;
    }
    let wide = 1 + usize::from(control) % count;
    let narrow = if wide == 1 { 2 } else { 1 + (wide - 1) / 2 };
    let (mut a, mut b) = (wide, narrow);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let step = wide / a * narrow;
    let n = count / step * step;
    (n != 0).then_some((wide, narrow, n))
}

/// Number of signatures actually checked under two different partitions.
pub fn check(data: &[u8]) -> usize {
    let Some((&control, stream)) = data.split_last() else {
        return 0;
    };
    let items = items(stream);
    let Some((wide, narrow, n)) = partition(items.len(), control) else {
        return 0;
    };
    let items = &items[..n];
    let seed = seed(data);
    let alone: Vec<_> = items
        .iter()
        .map(|item| item.0.clone().verify_single().is_ok())
        .collect();
    runtime().block_on(async {
        // Both partitions fill exactly, so timer flushes are not needed.
        let (w, n) = tokio::time::timeout(Duration::from_secs(10), async {
            (
                verdicts(items, wide, seed).await,
                verdicts(items, narrow, seed).await,
            )
        })
        .await
        .expect("bounded full batches complete without a timer flush");
        assert_no_false_accept(&w, &alone);
        assert_no_false_accept(&n, &alone);
        if alone.iter().all(|&ok| ok) {
            assert!(
                w.iter().chain(&n).all(|&ok| ok),
                "valid items rejected under a partition"
            );
        }
    });
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_worker_preserves_valid_signatures_and_rejects_poison() {
        let mut rng = rng(7);
        let mut items = Vec::new();
        for _ in 0..4 {
            let key = rj::SigningKey::<rj::SpendAuth>::new(&mut rng);
            let vk = rj::VerificationKey::from(&key);
            items.push(Item(
                (
                    rj::VerificationKeyBytes::from(vk),
                    key.sign(&mut rng, b"message"),
                    b"message",
                )
                    .into(),
            ));
        }
        runtime().block_on(async {
            for width in [1, 2, 4] {
                let result =
                    tokio::time::timeout(Duration::from_secs(5), verdicts(&items, width, 7))
                        .await
                        .unwrap();
                assert_eq!(result, vec![true; 4]);
            }
            let key = rj::SigningKey::<rj::SpendAuth>::new(&mut rng);
            items[1] = Item(
                (
                    rj::VerificationKeyBytes::from(rj::VerificationKey::from(&key)),
                    key.sign(&mut rng, b"different"),
                    b"message",
                )
                    .into(),
            );
            let alone: Vec<_> = items
                .iter()
                .map(|i| i.0.clone().verify_single().is_ok())
                .collect();
            assert_eq!(alone, vec![true, false, true, true]);
            for width in [1, 2, 4] {
                let result =
                    tokio::time::timeout(Duration::from_secs(5), verdicts(&items, width, 7))
                        .await
                        .unwrap();
                assert_no_false_accept(&result, &alone);
                assert!(!result[1]);
            }
        });
    }

    #[test]
    fn partitions_are_distinct_and_have_no_partial_tail() {
        for count in 0..=MAX_ITEMS {
            for control in 0..=255 {
                if let Some((w, n, used)) = partition(count, control) {
                    assert_ne!(w, n);
                    assert!(used > 0 && used <= count);
                    assert_eq!(used % w, 0);
                    assert_eq!(used % n, 0);
                }
            }
        }
    }
}
