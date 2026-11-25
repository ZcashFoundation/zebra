//! Ready transaction responses drain with bounded polls while the
//! cooperative budget stays active.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn ready_transaction_results_yield_without_repolling_the_backlog() {
    const COUNT: usize = 1024;

    let polls = Arc::new(AtomicUsize::new(0));
    let mut checks = FuturesUnordered::new();
    for index in 0..COUNT {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender.try_send(index).unwrap();
        let polls = polls.clone();
        checks.push(budget_aware(std::future::poll_fn(move |cx| {
            polls.fetch_add(1, Ordering::Relaxed);
            receiver.poll_recv(cx)
        })));
    }

    let other_task_ran = Arc::new(AtomicBool::new(false));
    let mut results = Vec::new();
    while let Some(result) = checks.next().await {
        results.push(result.unwrap());
        if results.len() == 1 {
            let other_task_ran = other_task_ran.clone();
            tokio::spawn(async move { other_task_ran.store(true, Ordering::SeqCst) });
        }
    }

    assert!(
        other_task_ran.load(Ordering::SeqCst),
        "draining must yield to another task"
    );
    results.sort_unstable();
    assert_eq!(results, (0..COUNT).collect::<Vec<_>>());
    assert!(
        polls.load(Ordering::Relaxed) <= 2 * COUNT,
        "budget exhaustion must not repeatedly poll already-ready checks"
    );
}
