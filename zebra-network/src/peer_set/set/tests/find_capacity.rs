//! Discovery queue capacity and cancellation controls retained from PR #11532.

use super::*;
use crate::peer_set::set::MAX_QUEUED_FIND_REQUESTS;

#[test]
fn peer_set_frees_queue_slot_of_cancelled_find() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();
    tokio::time::pause();

    runtime.block_on(async {
        for (request, _) in find_requests_and_responses() {
            let (mut peer_set, _peer_set_guard, addrs, mut handles, _best_tip) =
                peer_set_with_services(&SERVING_AND_NON_SERVING);
            let _busy = make_serving_peer_busy(&mut peer_set, addrs[0]).await;
            let mut queued = Vec::new();
            for _ in 0..MAX_QUEUED_FIND_REQUESTS {
                queued.push(
                    peer_set
                        .ready()
                        .await
                        .expect("fallback peer is ready")
                        .call(request.clone()),
                );
            }
            drop(queued.pop());
            let _replacement = peer_set
                .ready()
                .await
                .expect("fallback peer is ready")
                .call(request.clone());
            assert_eq!(
                received_request(&mut handles[1]),
                None,
                "cancelled find frees its slot"
            );
            let _overflow = peer_set
                .ready()
                .await
                .expect("fallback peer is ready")
                .call(request.clone());
            assert_eq!(
                received_request(&mut handles[1]),
                Some(request),
                "replacement consumes the freed slot"
            );
        }
    });
}
