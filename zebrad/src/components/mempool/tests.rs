use std::pin::Pin;

use proptest::prelude::*;
use tower::ServiceExt;

use super::{
    error::MempoolError, storage::Storage, ActiveState, InboundTxDownloads, Mempool, Request,
};
use crate::{
    components::sync::{RecentSyncLengths, SyncStatus},
    BoxError,
};
use zebra_chain::{
    amount::{Amount, NonNegative},
    parameters::NetworkKind,
    transparent::{self, Address},
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
};

mod prop;
mod vector;

impl Mempool {
    /// Get the storage field of the mempool for testing purposes.
    pub fn storage(&mut self) -> &mut Storage {
        match &mut self.active_state {
            ActiveState::Disabled => panic!("mempool must be enabled"),
            ActiveState::Enabled { storage, .. } => storage,
        }
    }

    /// Get the transaction downloader of the mempool for testing purposes.
    pub fn tx_downloads(&self) -> &Pin<Box<InboundTxDownloads>> {
        match &self.active_state {
            ActiveState::Disabled => panic!("mempool must be enabled"),
            ActiveState::Enabled { tx_downloads, .. } => tx_downloads,
        }
    }

    /// Enable the mempool by pretending the synchronization is close to the tip.
    ///
    /// Requires a chain tip action to enable the mempool before the future resolves.
    pub async fn enable(&mut self, recent_syncs: &mut RecentSyncLengths) {
        // Pretend we're close to tip
        SyncStatus::sync_close_to_tip(recent_syncs);
        // Make a dummy request to poll the mempool and make it enable itself
        self.dummy_call().await;
    }

    /// Disable the mempool by pretending the synchronization is far from the tip.
    pub async fn disable(&mut self, recent_syncs: &mut RecentSyncLengths) {
        // Pretend we're far from the tip
        SyncStatus::sync_far_from_tip(recent_syncs);
        // Make a dummy request to poll the mempool and make it disable itself
        self.dummy_call().await;
    }

    /// Perform a dummy service call so that `poll_ready` is called.
    pub async fn dummy_call(&mut self) {
        self.oneshot(Request::CheckForVerifiedTransactions)
            .await
            .expect("unexpected failure when checking for verified transactions");
    }
}

/// Helper trait to extract the [`MempoolError`] from a [`BoxError`].
pub trait UnboxMempoolError {
    /// Extract and unbox the [`MempoolError`] stored inside `self`.
    ///
    /// # Panics
    ///
    /// If the `boxed_error` is not a boxed [`MempoolError`].
    fn unbox_mempool_error(self) -> MempoolError;
}

impl UnboxMempoolError for MempoolError {
    fn unbox_mempool_error(self) -> MempoolError {
        self
    }
}

impl UnboxMempoolError for BoxError {
    fn unbox_mempool_error(self) -> MempoolError {
        self.downcast::<MempoolError>()
            .expect("error is not an expected `MempoolError`")
            // TODO: use `Box::into_inner` when it becomes stabilized.
            .as_ref()
            .clone()
    }
}

impl<T, E> UnboxMempoolError for Result<T, E>
where
    E: UnboxMempoolError,
{
    fn unbox_mempool_error(self) -> MempoolError {
        match self {
            Ok(_) => panic!("expected a mempool error, but got a success instead"),
            Err(error) => error.unbox_mempool_error(),
        }
    }
}

/// Return a [`VerifiedUnminedTx`] strategy with outputs and inputs adjusted to pass standardness.
pub fn standard_verified_unmined_tx_strategy() -> BoxedStrategy<VerifiedUnminedTx> {
    any::<Transaction>()
        .prop_map(|mut transaction| {
            standardize_transaction(&mut transaction);

            let unmined_tx = UnminedTx::from(transaction);
            let miner_fee = unmined_tx.conventional_fee;

            VerifiedUnminedTx::new(unmined_tx, miner_fee, 0)
                .expect("standardized transaction should pass ZIP-317 checks")
        })
        .boxed()
}

/// Mutate a transaction so its transparent inputs/outputs pass standardness checks.
pub fn standardize_transaction(transaction: &mut Transaction) {
    let lock_script = standard_lock_script();
    let output_value = Amount::<NonNegative>::try_from(10_000).expect("valid amount");

    for input in transaction.inputs_mut() {
        if let transparent::Input::PrevOut { unlock_script, .. } = input {
            *unlock_script = transparent::Script::new(&[]);
        }
    }

    for output in transaction.outputs_mut() {
        output.lock_script = lock_script.clone();
        output.value = output_value;
    }
}

fn standard_lock_script() -> transparent::Script {
    Address::from_pub_key_hash(NetworkKind::Mainnet, [0u8; 20]).script()
}
