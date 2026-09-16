//! Regression checks for retained look-ahead coinbase proofs.

use zcash_keys::address::Address;
use zebra_chain::parameters::NetworkUpgrade;
use zebra_rpc::config::mining::{default_miner_address, MinerAddressType};

use super::*;

fn parameters() -> (Network, MinerParams, Height) {
    let network = Network::Mainnet;
    let miner_params = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .expect("the hard-coded transparent address is valid"),
    );
    let height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 is active on mainnet");
    (network, miner_params, height)
}

/// A reorg must not detach a proof that can still be reused at its original height.
#[tokio::test]
async fn in_flight_coinbase_is_retained_across_height_changes() {
    let _init_guard = zebra_test::init();
    let (network, miner_params, height) = parameters();
    let other_height = height.next().expect("test height is below the maximum");
    let coinbase =
        TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
            .expect("test parameters produce a valid coinbase");
    let expected_coinbase = coinbase.clone();
    let cache = CoinbaseCache::default();
    let (release_proof, proof_released) = tokio::sync::oneshot::channel();
    let mut next_coinbase = Some((
        height,
        tokio::task::spawn_blocking(move || {
            proof_released
                .blocking_recv()
                .expect("the test releases the proof before awaiting it");
            coinbase
        }),
    ));

    timeout(Duration::from_secs(10), async {
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, other_height);
        release_proof.send(()).expect("the proof is still waiting");
        store_precomputed_coinbase(&mut next_coinbase, height, &cache).await;

        assert_eq!(cache.get(height, Amount::zero()), Some(expected_coinbase));
        assert!(cache.get(other_height, Amount::zero()).is_none());
    })
    .await
    .expect("the retained proof completes once released");
}

/// Completed stale work must neither enter the wrong cache entry nor prevent the next proof.
#[tokio::test]
async fn completed_coinbase_is_replaced_without_caching_the_wrong_height() {
    let _init_guard = zebra_test::init();
    let (network, miner_params, height) = parameters();
    let other_height = height.next().expect("test height is below the maximum");
    let cache = CoinbaseCache::default();
    let mut next_coinbase = None;
    start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, height);

    timeout(Duration::from_secs(10), async {
        while !next_coinbase
            .as_ref()
            .expect("a proof was started")
            .1
            .is_finished()
        {
            tokio::task::yield_now().await;
        }
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        assert!(cache.get(height, Amount::zero()).is_none());
        assert!(cache.get(other_height, Amount::zero()).is_none());

        start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, other_height);
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        assert_eq!(
            cache.get(other_height, Amount::zero()),
            Some(
                TransactionTemplate::new_coinbase(
                    &network,
                    other_height,
                    &miner_params,
                    Amount::zero(),
                )
                .expect("test parameters produce a valid coinbase")
            ),
        );
    })
    .await
    .expect("the replacement proof completes");
}
