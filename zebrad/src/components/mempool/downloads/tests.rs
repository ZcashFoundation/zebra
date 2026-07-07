//! Fixed test vectors for the mempool transaction downloader.

use zebra_chain::parameters::Network;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::*;

/// A directly pushed full transaction must consume the sending peer's
/// per-peer admission budget, exactly like an advertised transaction ID, so
/// a single peer cannot bypass [`MAX_INBOUND_CONCURRENCY_PER_PEER`] by
/// sending `tx` messages instead of `inv` advertisements.
///
/// Regression test for `GHSA-m9xx-8rcj-vmgp`.
#[tokio::test]
async fn per_peer_cap_applies_to_pushed_transactions() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let source: SocketAddr = "192.168.0.1:8233".parse().expect("valid socket address");

    // Distinct full transactions, as if pushed directly by a single peer.
    let pushed: Vec<Gossip> = network
        .unmined_transactions_in_blocks(1..=10)
        .take(MAX_INBOUND_CONCURRENCY_PER_PEER + 1)
        .map(|tx| Gossip::Tx(tx.transaction))
        .collect();
    assert_eq!(
        pushed.len(),
        MAX_INBOUND_CONCURRENCY_PER_PEER + 1,
        "test needs enough distinct transactions to exceed the per-peer cap",
    );

    // The cap is enforced before any download/verify work, so the services
    // are never actually driven in this test.
    let peer_set: MockService<zn::Request, zn::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let verifier: MockService<tx::Request, tx::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let state: MockService<zs::Request, zs::Response, PanicAssertion> =
        MockService::build().for_unit_tests();

    let mut downloads = Downloads::new(peer_set, verifier, state);

    // The first `MAX_INBOUND_CONCURRENCY_PER_PEER` pushes from this peer are admitted.
    for gossip in pushed.iter().take(MAX_INBOUND_CONCURRENCY_PER_PEER) {
        downloads
            .download_if_needed_and_verify(gossip.clone(), Some(source), None)
            .expect("pushes below the per-peer cap are admitted");
    }

    // The next push from the same peer exceeds the per-peer cap and is
    // rejected, even though the global queue still has spare capacity.
    let result = downloads.download_if_needed_and_verify(
        pushed[MAX_INBOUND_CONCURRENCY_PER_PEER].clone(),
        Some(source),
        None,
    );
    assert!(
        matches!(result, Err(MempoolError::FullQueue)),
        "a push exceeding the per-peer cap must be rejected with `FullQueue`, got {result:?}",
    );

    // Don't leave spawned download tasks behind when the runtime shuts down.
    downloads.cancel_all();
}
