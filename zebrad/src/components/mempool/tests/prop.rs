use proptest::prelude::*;
use tokio::time;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{parameters::Network, transaction::UnminedTx};
use zebra_consensus::{error::TransactionError, transaction as tx};
use zebra_network as zn;
use zebra_state::{self as zs, ChainTipBlock, ChainTipSender};
use zebra_test::mock_service::{MockService, PropTestAssertion};

use super::super::Mempool;
use crate::components::sync::{RecentSyncLengths, SyncStatus};

/// A [`MockService`] representing the network service.
type MockPeerSet = MockService<zn::Request, zn::Response, PropTestAssertion>;

/// A [`MockService`] representing the Zebra state service.
type MockState = MockService<zs::Request, zs::Response, PropTestAssertion>;

/// A [`MockService`] representing the Zebra transaction verifier service.
type MockTxVerifier = MockService<tx::Request, tx::Response, PropTestAssertion, TransactionError>;

proptest! {
    /// Test if the mempool storage is cleared on a chain reset.
    #[test]
    fn storage_is_cleared_on_chain_reset(
        network in any::<Network>(),
        transaction in any::<UnminedTx>(),
        chain_tip in any::<ChainTipBlock>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");
        let _guard = runtime.enter();

        runtime.block_on(async move {
            let (
                mut mempool,
                mut peer_set,
                mut state_service,
                mut tx_verifier,
                mut recent_syncs,
                mut chain_tip_sender,
            ) = setup(network);

            time::pause();

            mempool.enable(&mut recent_syncs).await;

            // Insert a dummy transaction.
            mempool
                .storage()
                .insert(transaction)
                .expect("Inserting a transaction should succeed");

            // The first call to `poll_ready` shouldn't clear the storage yet.
            mempool.dummy_call().await;

            prop_assert_eq!(mempool.storage().tx_ids().len(), 1);

            // Simulate a chain reset.
            chain_tip_sender.set_finalized_tip(chain_tip);

            // This time a call to `poll_ready` should clear the storage.
            mempool.dummy_call().await;

            prop_assert!(mempool.storage().tx_ids().is_empty());

            peer_set.expect_no_requests().await?;
            state_service.expect_no_requests().await?;
            tx_verifier.expect_no_requests().await?;

            Ok(())
        })?;
    }
}

/// Create a new [`Mempool`] instance using mocked services.
fn setup(
    network: Network,
) -> (
    Mempool,
    MockPeerSet,
    MockState,
    MockTxVerifier,
    RecentSyncLengths,
    ChainTipSender,
) {
    let peer_set = MockService::build().for_prop_tests();
    let state_service = MockService::build().for_prop_tests();
    let tx_verifier = MockService::build().for_prop_tests();

    let (sync_status, recent_syncs) = SyncStatus::new();
    let (chain_tip_sender, latest_chain_tip, chain_tip_change) = ChainTipSender::new(None, network);

    let mempool = Mempool::new(
        network,
        Buffer::new(BoxService::new(peer_set.clone()), 1),
        Buffer::new(BoxService::new(state_service.clone()), 1),
        Buffer::new(BoxService::new(tx_verifier.clone()), 1),
        sync_status,
        latest_chain_tip,
        chain_tip_change,
    );

    (
        mempool,
        peer_set,
        state_service,
        tx_verifier,
        recent_syncs,
        chain_tip_sender,
    )
}
