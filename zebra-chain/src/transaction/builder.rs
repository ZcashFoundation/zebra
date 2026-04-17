//! Methods for building transactions.

use zcash_primitives::transaction::{self as zp_tx, TxVersion};
use zcash_protocol::consensus::BranchId;

use crate::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade},
    transaction::{compat, LockTime, Transaction},
    transparent,
};

impl Transaction {
    /// Returns a new version 5 coinbase transaction for `network` and `height`,
    /// which contains the specified `outputs`.
    pub fn new_v5_coinbase(
        network: &Network,
        height: Height,
        outputs: impl IntoIterator<Item = (Amount<NonNegative>, transparent::Script)>,
        miner_data: Vec<u8>,
    ) -> Transaction {
        let inputs = [transparent::Input::new_coinbase(height, miner_data, None)];
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(amount, lock_script)| transparent::Output::new(amount, lock_script))
            .collect();

        assert!(
            !outputs.is_empty(),
            "invalid coinbase transaction: must have at least one output"
        );

        let nu = NetworkUpgrade::current(network, height);
        let branch_id = nu
            .branch_id()
            .and_then(|cbid| BranchId::try_from(cbid).ok())
            .expect("V5 coinbase requires a valid consensus branch ID");

        let vin: Vec<_> = inputs.iter().map(compat::input_to_txin).collect();
        let vout: Vec<_> = outputs.iter().map(compat::output_to_txout).collect();

        let transparent_bundle = Some(zcash_transparent::bundle::Bundle {
            vin,
            vout,
            authorization: zcash_transparent::bundle::Authorized,
        });

        let tx_data = zp_tx::TransactionData::from_parts(
            TxVersion::V5,
            branch_id,
            compat::lock_time_to_u32(&LockTime::unlocked()),
            height.into(),
            zp_tx::zip248::ValuePoolDeltas::default(),
            transparent_bundle,
            None, // sprout
            None, // sapling
            None, // orchard
        );

        let inner = tx_data
            .freeze()
            .expect("valid V5 coinbase transaction should freeze successfully");

        Transaction(inner)
    }

    /// Returns a new version 4 coinbase transaction for `network` and `height`,
    /// which contains the specified `outputs`.
    pub fn new_v4_coinbase(
        height: Height,
        outputs: impl IntoIterator<Item = (Amount<NonNegative>, transparent::Script)>,
        miner_data: Vec<u8>,
    ) -> Transaction {
        let inputs = [transparent::Input::new_coinbase(height, miner_data, None)];
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(amount, lock_script)| transparent::Output::new(amount, lock_script))
            .collect();

        assert!(
            !outputs.is_empty(),
            "invalid coinbase transaction: must have at least one output"
        );

        let branch_id = BranchId::Canopy;

        let vin: Vec<_> = inputs.iter().map(compat::input_to_txin).collect();
        let vout: Vec<_> = outputs.iter().map(compat::output_to_txout).collect();

        let transparent_bundle = Some(zcash_transparent::bundle::Bundle {
            vin,
            vout,
            authorization: zcash_transparent::bundle::Authorized,
        });

        let tx_data = zp_tx::TransactionData::from_parts(
            TxVersion::V4,
            branch_id,
            compat::lock_time_to_u32(&LockTime::unlocked()),
            height.into(),
            zp_tx::zip248::ValuePoolDeltas::default(),
            transparent_bundle,
            None, // sprout
            None, // sapling
            None, // orchard
        );

        let inner = tx_data
            .freeze()
            .expect("valid V4 coinbase transaction should freeze successfully");

        Transaction(inner)
    }
}
