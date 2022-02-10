//! Randomised property tests for the mempool.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

use chrono::Duration;
use tokio::time;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    block,
    parameters::{Network, NetworkUpgrade},
    transaction::VerifiedUnminedTx,
};
use zebra_consensus::{error::TransactionError, transaction as tx};
use zebra_network as zn;
use zebra_state::{self as zs, ChainTipBlock, ChainTipSender};
use zebra_test::mock_service::{MockService, PropTestAssertion};

use crate::components::{
    mempool::{config::Config, Mempool},
    sync::{RecentSyncLengths, SyncStatus},
};

/// A [`MockService`] representing the network service.
type MockPeerSet = MockService<zn::Request, zn::Response, PropTestAssertion>;

/// A [`MockService`] representing the Zebra state service.
type MockState = MockService<zs::Request, zs::Response, PropTestAssertion>;

/// A [`MockService`] representing the Zebra transaction verifier service.
type MockTxVerifier = MockService<tx::Request, tx::Response, PropTestAssertion, TransactionError>;

const CHAIN_LENGTH: usize = 10;

proptest! {
    /// Test if the mempool storage is cleared on a chain reset.
    #[test]
    fn storage_is_cleared_on_chain_reset(
        network in any::<Network>(),
        transaction in any::<VerifiedUnminedTx>(),
        chain_tip in any::<ChainTipBlock>(),
    ) {
        let runtime = zebra_test::init_async();

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

            prop_assert_eq!(mempool.storage().transaction_count(), 1);

            // Simulate a chain reset.
            chain_tip_sender.set_finalized_tip(chain_tip);

            // This time a call to `poll_ready` should clear the storage.
            mempool.dummy_call().await;

            prop_assert_eq!(mempool.storage().transaction_count(), 0);

            peer_set.expect_no_requests().await?;
            state_service.expect_no_requests().await?;
            tx_verifier.expect_no_requests().await?;

            Ok(())
        })?;
    }

    /// Test if the mempool storage is cleared on multiple chain resets.
    #[test]
    fn storage_is_cleared_on_chain_resets(
        network in any::<Network>(),
        mut previous_chain_tip in any::<ChainTipBlock>(),
        mut transactions in vec(any::<VerifiedUnminedTx>(), 0..CHAIN_LENGTH),
        fake_chain_tips in vec(any::<FakeChainTip>(), 0..CHAIN_LENGTH),
    ) {
        let runtime = zebra_test::init_async();

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

            // Set the initial chain tip.
            chain_tip_sender.set_best_non_finalized_tip(previous_chain_tip.clone());

            // Call the mempool so that it is aware of the initial chain tip.
            mempool.dummy_call().await;

            for (fake_chain_tip, transaction) in fake_chain_tips.iter().zip(transactions.iter_mut()) {
                // Obtain a new chain tip based on the previous one.
                let chain_tip = fake_chain_tip.to_chain_tip_block(&previous_chain_tip, network);

                // Adjust the transaction expiry height based on the new chain
                // tip height so that the mempool does not evict the transaction
                // when there is a chain growth.
                if let Some(expiry_height) = transaction.transaction.transaction.expiry_height() {
                    if chain_tip.height >= expiry_height {
                        let mut tmp_tx = (*transaction.transaction.transaction).clone();

                        // Set a new expiry height that is greater than the
                        // height of the current chain tip.
                        *tmp_tx.expiry_height_mut() = block::Height(chain_tip.height.0 + 1);
                        transaction.transaction = tmp_tx.into();
                    }
                }

                // Insert the dummy transaction into the mempool.
                mempool
                    .storage()
                    .insert(transaction.clone())
                    .expect("Inserting a transaction should succeed");

                // Set the new chain tip.
                chain_tip_sender.set_best_non_finalized_tip(chain_tip.clone());

                // Call the mempool so that it is aware of the new chain tip.
                mempool.dummy_call().await;

                match fake_chain_tip {
                    FakeChainTip::Grow(_) => {
                        // The mempool should not be empty because we had a regular chain growth.
                        prop_assert_ne!(mempool.storage().transaction_count(), 0);
                    }

                    FakeChainTip::Reset(_) => {
                        // The mempool should be empty because we had a chain tip reset.
                        prop_assert_eq!(mempool.storage().transaction_count(), 0);
                    },
                }

                // Remember the current chain tip so that the next one can refer to it.
                previous_chain_tip = chain_tip;
            }

            peer_set.expect_no_requests().await?;
            state_service.expect_no_requests().await?;
            tx_verifier.expect_no_requests().await?;

            Ok(())
        })?;
    }

    /// Test if the mempool storage is cleared if the syncer falls behind and starts to catch up.
    #[test]
    fn storage_is_cleared_if_syncer_falls_behind(
        network in any::<Network>(),
        transaction in any::<VerifiedUnminedTx>(),
    ) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let (
                mut mempool,
                mut peer_set,
                mut state_service,
                mut tx_verifier,
                mut recent_syncs,
                _chain_tip_sender,
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

            prop_assert_eq!(mempool.storage().transaction_count(), 1);

            // Simulate the synchronizer catching up to the network chain tip.
            mempool.disable(&mut recent_syncs).await;

            // This time a call to `poll_ready` should clear the storage.
            mempool.dummy_call().await;

            // Enable the mempool again so the storage can be accessed.
            mempool.enable(&mut recent_syncs).await;

            prop_assert_eq!(mempool.storage().transaction_count(), 0);

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

    let (mempool, _transaction_receiver) = Mempool::new(
        &Config {
            tx_cost_limit: 160_000_000,
            ..Default::default()
        },
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

/// A helper enum for simulating either a chain reset or growth.
#[derive(Arbitrary, Debug, Clone)]
enum FakeChainTip {
    Grow(ChainTipBlock),
    Reset(ChainTipBlock),
}

impl FakeChainTip {
    /// Returns a new [`ChainTipBlock`] placed on top of the previous block if
    /// the chain is supposed to grow. Otherwise returns a [`ChainTipBlock`]
    /// that does not reference the previous one.
    fn to_chain_tip_block(&self, previous: &ChainTipBlock, network: Network) -> ChainTipBlock {
        match self {
            Self::Grow(chain_tip_block) => {
                let height = block::Height(previous.height.0 + 1);
                let target_spacing = NetworkUpgrade::target_spacing_for_height(network, height);

                let mock_block_time_delta = Duration::seconds(
                    previous.time.timestamp() % (2 * target_spacing.num_seconds()),
                );

                ChainTipBlock {
                    hash: chain_tip_block.hash,
                    height,
                    time: previous.time + mock_block_time_delta,
                    transaction_hashes: chain_tip_block.transaction_hashes.clone(),
                    previous_block_hash: previous.hash,
                }
            }

            Self::Reset(chain_tip_block) => chain_tip_block.clone(),
        }
    }
}
