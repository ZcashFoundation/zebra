//! Transparent address indexes for non-finalized chains.

use std::collections::{HashMap, HashSet};

use mset::MultiSet;

use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    transaction, transparent,
};

use crate::{OutputLocation, ValidateContextError};

use super::{RevertPosition, UpdateWith};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparentTransfers {
    /// The partial chain balance for a transparent address.
    ///
    /// TODO:
    /// To avoid [`ReadStateService`] response inconsistencies when a block has just been finalized,
    /// revert UTXO receives and spends that are at a height less than or equal to the finalized tip.
    balance: Amount<NegativeAllowed>,

    /// The partial list of transactions that spent or received UTXOs to a transparent address.
    ///
    /// Since transactions can only be added to this set, it does not need special handling
    /// for [`ReadStateService`] response inconsistencies.
    ///
    /// The `getaddresstxids` RPC needs these transaction IDs to be sorted in chain order.
    ///
    /// TODO: return as a BTreeMap<TransactionLocation, transaction::Hash>,
    ///       so transactions are in chain order
    ///
    ///       replace tuple value with TransactionLocation, and look up the hash in hash_by_tx
    ///
    ///       use Arc<Hash> to save 24 bytes per transaction
    //
    // TODO: use (TransactionLocation, transaction::Hash) as part of PR #3978
    tx_ids: MultiSet<transaction::Hash>,

    /// The partial list of UTXOs received by a transparent address.
    ///
    /// The `getaddressutxos` RPC doesn't need these transaction IDs to be sorted in chain order,
    /// but it might in future. So Zebra does it anyway.
    ///
    /// TODO:
    /// To avoid [`ReadStateService`] response inconsistencies when a block has just been finalized,
    /// ignore UTXOs that are at a height less than or equal to the finalized tip.
    ///
    /// TODO: use Arc<Utxo> to save 2-100 bytes per output
    ///
    ///       if we add an OutputLocation to UTXO, remove this OutputLocation,
    ///       and use the inner OutputLocation to sort Utxos in chain order
    //
    // TODO: use BTreeMap as part of PR #3978
    created_utxos: HashMap<OutputLocation, transparent::Utxo>,

    /// The partial list of UTXOs spent by a transparent address.
    ///
    /// The `getaddressutxos` RPC doesn't need these transaction IDs to be sorted in chain order,
    /// but it might in future. So Zebra does it anyway.
    ///
    /// TODO:
    /// To avoid [`ReadStateService`] response inconsistencies when a block has just been finalized,
    /// ignore UTXOs that are at a height less than or equal to the finalized tip.
    ///
    /// TODO: use Arc<Utxo> to save 2-100 bytes per output
    ///
    ///       if we add an OutputLocation to UTXO, remove this OutputLocation,
    ///       and use the inner OutputLocation to sort Utxos in chain order
    //
    // TODO: use BTreeSet as part of PR #3978
    spent_utxos: HashSet<OutputLocation>,
}

// A created UTXO
impl UpdateWith<(&transparent::OutPoint, &transparent::Utxo)> for TransparentTransfers {
    fn update_chain_tip_with(
        &mut self,
        &(outpoint, utxo): &(&transparent::OutPoint, &transparent::Utxo),
    ) -> Result<(), ValidateContextError> {
        self.balance = (self.balance + utxo.output.value().constrain().unwrap()).unwrap();

        // TODO: lookup height and transaction index as part of PR #3978
        let output_location = OutputLocation::from_outpoint(outpoint);
        assert_eq!(
            self.created_utxos.insert(output_location, utxo.clone()),
            None,
            "unexpected created UTXO: duplicate update or duplicate creation",
        );

        // TODO: store TransactionLocation as part of PR #3978
        self.tx_ids.insert(outpoint.hash);

        Ok(())
    }

    fn revert_chain_with(
        &mut self,
        &(outpoint, utxo): &(&transparent::OutPoint, &transparent::Utxo),
        _position: RevertPosition,
    ) {
        self.balance = (self.balance - utxo.output.value().constrain().unwrap()).unwrap();

        // TODO: lookup height and transaction index as part of PR #3978
        let output_location = OutputLocation::from_outpoint(outpoint);
        assert!(
            self.created_utxos.remove(&output_location).is_some(),
            "unexpected created UTXO: duplicate revert, or revert of an output that was never updated",
        );

        assert!(
            self.tx_ids.remove(&outpoint.hash),
            "unexpected created UTXO transaction hash: duplicate revert, or revert that was never updated",
        );
    }
}

// A spending input, the output it spends, and the transaction that spends it
impl
    UpdateWith<(
        &transparent::Input,
        &transparent::Output,
        &transaction::Hash,
    )> for TransparentTransfers
{
    fn update_chain_tip_with(
        &mut self,
        &(spending_input, spent_output, spending_tx): &(
            &transparent::Input,
            &transparent::Output,
            &transaction::Hash,
        ),
    ) -> Result<(), ValidateContextError> {
        // Spending a UTXO subtracts value from the balance
        self.balance = (self.balance - spent_output.value().constrain().unwrap()).unwrap();

        let spent_outpoint = spending_input.outpoint().expect("checked by caller");

        // TODO: lookup height and transaction index as part of PR #3978
        let output_location = OutputLocation::from_outpoint(&spent_outpoint);
        assert!(
            self.spent_utxos.insert(output_location),
            "unexpected spent output: duplicate update or duplicate spend",
        );

        // TODO: store TransactionLocation as part of PR #3978
        self.tx_ids.insert(*spending_tx);

        Ok(())
    }

    fn revert_chain_with(
        &mut self,
        &(spending_input, spent_output, spending_tx): &(
            &transparent::Input,
            &transparent::Output,
            &transaction::Hash,
        ),
        _position: RevertPosition,
    ) {
        self.balance = (self.balance + spent_output.value().constrain().unwrap()).unwrap();

        let spent_outpoint = spending_input.outpoint().expect("checked by caller");

        // TODO: lookup height and transaction index as part of PR #3978
        let output_location = OutputLocation::from_outpoint(&spent_outpoint);
        assert!(
            self.spent_utxos.remove(&output_location),
            "unexpected spent output: duplicate revert, or revert of an output that was never updated",
        );

        assert!(
            self.tx_ids.remove(spending_tx),
            "unexpected spending output transaction hash: duplicate revert, or revert that was never updated",
        );
    }
}

impl TransparentTransfers {
    /// Returns true if there are no transfers for this address.
    pub fn is_empty(&self) -> bool {
        self.balance == Amount::<NegativeAllowed>::zero()
            && self.tx_ids.is_empty()
            && self.created_utxos.is_empty()
            && self.spent_utxos.is_empty()
    }
}

impl Default for TransparentTransfers {
    fn default() -> Self {
        Self {
            balance: Amount::zero(),
            tx_ids: Default::default(),
            created_utxos: Default::default(),
            spent_utxos: Default::default(),
        }
    }
}
