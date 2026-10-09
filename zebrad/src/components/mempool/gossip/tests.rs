//! Lag recovery, bounded batches, and independent transaction gossip pacing.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::{mpsc, Mutex};
use tower::service_fn;
use zebra_chain::transaction::{Hash, UnminedTxId};

use super::*;

fn ids(count: usize) -> HashSet<UnminedTxId> {
    (0..count)
        .map(|index| {
            let mut hash = [0; 32];
            hash[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
            UnminedTxId::Legacy(Hash(hash))
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn pending_ids_drain_without_notifications_in_paced_bounded_batches() {
    let limit = usize::try_from(zn::MAX_TX_INV_IN_SENT_MESSAGE).unwrap();
    let expected = ids(limit + 1);
    let pending = Arc::new(Mutex::new(expected.clone()));
    let mempool = service_fn(move |request| {
        let Request::TakePendingGossipTransactionIds { limit } = request else {
            panic!("unexpected request")
        };
        let pending = pending.clone();
        async move {
            let mut pending = pending.lock().await;
            let batch: HashSet<_> = pending.iter().copied().take(limit).collect();
            for id in &batch {
                pending.remove(id);
            }
            Ok::<_, BoxError>(Response::TransactionIds(batch))
        }
    });
    let (sent, mut received) = mpsc::channel(4);
    let network = service_fn(move |request| {
        let sent = sent.clone();
        async move {
            sent.send((tokio::time::Instant::now(), request))
                .await
                .unwrap();
            Ok::<_, BoxError>(zn::Response::Nil)
        }
    });
    let (_sender, receiver) = broadcast::channel(1);
    let task = tokio::spawn(gossip_mempool_transaction_id(receiver, network, mempool));
    let (first_time, first) = tokio::time::timeout(Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    let zn::Request::AdvertiseTransactionIds(first, None) = first else {
        panic!("expected transaction INV")
    };
    assert_eq!(first.len(), limit);
    assert!(
        received.try_recv().is_err(),
        "the next batch must wait for the gossip delay"
    );
    let (second_time, second) = tokio::time::timeout(Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    let zn::Request::AdvertiseTransactionIds(second, None) = second else {
        panic!("expected transaction INV")
    };
    assert_eq!(second.len(), 1);
    assert!(second_time - first_time >= TRANSACTION_GOSSIP_DELAY);
    assert_eq!(
        first.union(&second).copied().collect::<HashSet<_>>(),
        expected
    );
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn lagged_wakeups_and_network_failure_do_not_lose_accepted_ids() {
    let expected = ids(5);
    let pending = Arc::new(Mutex::new(HashSet::new()));
    let pending_service = pending.clone();
    let (drained, mut drain_observer) = mpsc::channel(8);
    let mempool = service_fn(move |request| {
        let Request::TakePendingGossipTransactionIds { .. } = request else {
            panic!("unexpected request")
        };
        let pending = pending_service.clone();
        let drained = drained.clone();
        async move {
            let batch = std::mem::take(&mut *pending.lock().await);
            drained.send(()).await.unwrap();
            Ok::<_, BoxError>(Response::TransactionIds(batch))
        }
    });
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_service = attempts.clone();
    let (sent, mut received) = mpsc::channel(4);
    let network = service_fn(move |request| {
        let sent = sent.clone();
        let fail = attempts_service.fetch_add(1, Ordering::SeqCst) == 0;
        async move {
            if fail {
                return Err::<zn::Response, BoxError>("temporary send failure".into());
            }
            sent.send(request).await.unwrap();
            Ok(zn::Response::Nil)
        }
    });
    let (sender, receiver) = broadcast::channel(1);
    let task = tokio::spawn(gossip_mempool_transaction_id(receiver, network, mempool));
    tokio::time::timeout(Duration::from_secs(10), drain_observer.recv())
        .await
        .unwrap()
        .unwrap();
    pending.lock().await.extend(expected.iter().copied());
    // The retained notification is not even an addition: lag itself must trigger recovery.
    sender.send(MempoolChange::added(expected.clone())).unwrap();
    sender
        .send(MempoolChange::invalidated(HashSet::new()))
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    let zn::Request::AdvertiseTransactionIds(actual, None) = request else {
        panic!("expected transaction INV")
    };
    assert_eq!(actual, expected);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    task.abort();
}
