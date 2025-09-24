//! Randomised property tests for the mempool.

use std::{env, fmt, sync::Arc, time::Duration as StdDuration};

use proptest::{collection::vec, prelude::*};
use proptest_derive::Arbitrary;

use chrono::Duration;
use tokio::{sync::watch, time};
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    block::{self, Block},
    fmt::{DisplayToDebug, TypeNameToDebug},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::VerifiedUnminedTx,
};
use zebra_consensus::{error::TransactionError, transaction as tx};
use zebra_network::{self as zn, PeerSetStatus};
use zebra_state::{self as zs, ChainTipBlock, ChainTipSender};
use zebra_test::mock_service::{MockService, PropTestAssertion};
use zs::CheckpointVerifiedBlock;

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

const CHAIN_LENGTH: usize = 5;

const DEFAULT_MEMPOOL_PROPTEST_CASES: u32 = 8;

proptest! {
    // The mempool tests can generate very verbose logs, so we use fewer cases by
    // default. Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_MEMPOOL_PROPTEST_CASES)))]

    /// Test if the mempool storage is cleared on a chain reset.
    #[test]
    fn storage_is_cleared_on_single_chain_reset(
        network in any::<Network>(),
        transaction in any::<DisplayToDebug<VerifiedUnminedTx>>(),
        chain_tip in any::<DisplayToDebug<ChainTipBlock>>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let (
                mut mempool,
                _peer_set,
                _state_service,
                _tx_verifier,
                mut recent_syncs,
                mut chain_tip_sender,
            ) = setup(&network);

            time::pause();

            mempool.enable(&mut recent_syncs).await;

            // Insert a dummy transaction.
            mempool
                .storage()
                .insert(transaction.0, Vec::new(), None)
                .expect("Inserting a transaction should succeed");

            // The first call to `poll_ready` shouldn't clear the storage yet.
            mempool.dummy_call().await;

            prop_assert_eq!(mempool.storage().transaction_count(), 1);

            // Simulate a chain reset.
            chain_tip_sender.set_finalized_tip(chain_tip.0);

            // This time a call to `poll_ready` should clear the storage.
            mempool.dummy_call().await;

            prop_assert_eq!(mempool.storage().transaction_count(), 0);

            // The services might or might not get requests,
            // depending on how many transactions get re-queued, and if they need downloading.

            Ok(())
        })?;
    }

    /// Test if the mempool storage is cleared on multiple chain resets.
    #[test]
    fn storage_is_cleared_on_multiple_chain_resets(
        network in any::<Network>(),
        mut previous_chain_tip in any::<DisplayToDebug<ChainTipBlock>>(),
        mut transactions in vec(any::<DisplayToDebug<VerifiedUnminedTx>>(), 0..CHAIN_LENGTH),
        fake_chain_tips in vec(any::<TypeNameToDebug<FakeChainTip>>(), 0..CHAIN_LENGTH),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let (
                mut mempool,
                _peer_set,
                _state_service,
                _tx_verifier,
                mut recent_syncs,
                mut chain_tip_sender,
            ) = setup(&network);

            time::pause();

            mempool.enable(&mut recent_syncs).await;

            // Set the initial chain tip.
            chain_tip_sender.set_best_non_finalized_tip(previous_chain_tip.0.clone());

            // Call the mempool so that it is aware of the initial chain tip.
            mempool.dummy_call().await;

            for (fake_chain_tip, transaction) in fake_chain_tips.iter().zip(transactions.iter_mut()) {
                // Obtain a new chain tip based on the previous one.
                let chain_tip = fake_chain_tip.to_chain_tip_block(&previous_chain_tip, &network);

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
                    .insert(transaction.0.clone(), Vec::new(), None)
                    .expect("Inserting a transaction should succeed");

                // Set the new chain tip.
                chain_tip_sender.set_best_non_finalized_tip(chain_tip.clone());

                // Call the mempool so that it is aware of the new chain tip.
                mempool.dummy_call().await;

                match fake_chain_tip.0 {
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
                previous_chain_tip = chain_tip.into();
            }

            // The services might or might not get requests,
            // depending on how many transactions get re-queued, and if they need downloading.

            Ok(())
        })?;
    }

    /// Test if the mempool storage is cleared if the syncer falls behind and starts to catch up.
    #[test]
    fn storage_is_cleared_if_syncer_falls_behind(
        network in any::<Network>(),
        transaction in any::<VerifiedUnminedTx>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let (
                mut mempool,
                mut peer_set,
                mut state_service,
                mut tx_verifier,
                mut recent_syncs,
                mut chain_tip_sender,
            ) = setup(&network);

            time::pause();

            mempool.enable(&mut recent_syncs).await;

            // Insert a dummy transaction.
            mempool
                .storage()
                .insert(transaction, Vec::new(), None)
                .expect("Inserting a transaction should succeed");

            // The first call to `poll_ready` shouldn't clear the storage yet.
            mempool.dummy_call().await;

            prop_assert_eq!(mempool.storage().transaction_count(), 1);

            // Simulate the synchronizer catching up to the network chain tip.
            mempool.disable(&mut recent_syncs).await;

            // This time a call to `poll_ready` should clear the storage.
            mempool.dummy_call().await;

            // sends a new fake chain tip so that the mempool can be enabled
            chain_tip_sender.set_finalized_tip(block1_chain_tip());

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

fn genesis_chain_tip() -> Option<ChainTipBlock> {
    zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .map(CheckpointVerifiedBlock::from)
        .map(ChainTipBlock::from)
        .ok()
}

fn block1_chain_tip() -> Option<ChainTipBlock> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .map(CheckpointVerifiedBlock::from)
        .map(ChainTipBlock::from)
        .ok()
}

/// Create a new [`Mempool`] instance using mocked services.
fn setup(
    network: &Network,
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

    let (mut chain_tip_sender, latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, network);

    let (peer_status_tx, peer_status_rx) = watch::channel(PeerSetStatus::default());
    let min_ready_peers = if network.is_regtest() { 0 } else { 1 };
    if min_ready_peers > 0 {
        let _ = peer_status_tx.send(PeerSetStatus::new(min_ready_peers, min_ready_peers));
    }

    let (sync_status, recent_syncs) = SyncStatus::new(
        peer_status_rx,
        latest_chain_tip.clone(),
        min_ready_peers,
        StdDuration::from_secs(5 * 60),
    );

    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (mempool, mempool_transaction_subscriber) = Mempool::new(
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
        misbehavior_tx,
    );

    let mut transaction_receiver = mempool_transaction_subscriber.subscribe();
    tokio::spawn(async move { while transaction_receiver.recv().await.is_ok() {} });

    // sends a fake chain tip so that the mempool can be enabled
    chain_tip_sender.set_finalized_tip(genesis_chain_tip());

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
#[derive(Arbitrary, Clone, Debug, Eq, PartialEq)]
enum FakeChainTip {
    Grow(ChainTipBlock),
    Reset(ChainTipBlock),
}

impl fmt::Display for FakeChainTip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (mut f, inner) = match self {
            FakeChainTip::Grow(inner) => (f.debug_tuple("FakeChainTip::Grow"), inner),
            FakeChainTip::Reset(inner) => (f.debug_tuple("FakeChainTip::Reset"), inner),
        };

        f.field(&inner).finish()
    }
}

impl FakeChainTip {
    /// Returns a new [`ChainTipBlock`] placed on top of the previous block if
    /// the chain is supposed to grow. Otherwise returns a [`ChainTipBlock`]
    /// that does not reference the previous one.
    fn to_chain_tip_block(&self, previous: &ChainTipBlock, network: &Network) -> ChainTipBlock {
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
                    transactions: chain_tip_block.transactions.clone(),
                    transaction_hashes: chain_tip_block.transaction_hashes.clone(),
                    previous_block_hash: previous.hash,
                }
            }

            Self::Reset(chain_tip_block) => chain_tip_block.clone(),
        }
    }
}
