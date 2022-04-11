//! Transparent address indexes for non-finalized chains.

use std::collections::{HashMap, HashSet};

use mset::MultiSet;

use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    transaction, transparent,
};

use crate::{OutputLocation, TransactionLocation, ValidateContextError};

use super::{RevertPosition, UpdateWith};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparentTransfers {
    /// The partial chain balance for a transparent address.
    ///
    /// TODO:
    /// - to avoid [`ReadStateService`] response inconsistencies when a block has just been finalized,
    ///   revert UTXO receives and spends that are at a height less than or equal to the finalized tip.
    balance: Amount<NegativeAllowed>,

    /// The partial list of transactions that spent or received UTXOs to a transparent address.
    ///
    /// Since transactions can only be added to this set, it does not need special handling
    /// for [`ReadStateService`] response inconsistencies.
    ///
    /// The `getaddresstxids` RPC needs these transaction IDs to be sorted in chain order.
    ///
    /// TODO:
    /// - use (TransactionLocation, transaction::Hash) as part of PR #3978
    /// - return as a BTreeMap<TransactionLocation, transaction::Hash>, so transactions are in chain order
    ///
    /// Optional:
    /// - replace tuple value with TransactionLocation, and look up the hash in hash_by_tx
    tx_ids: MultiSet<transaction::Hash>,

    /// The partial list of UTXOs received by a transparent address.
    ///
    /// The `getaddressutxos` RPC doesn't need these transaction IDs to be sorted in chain order,
    /// but it might in future. So Zebra does it anyway.
    ///
    /// TODO:
    /// - use BTreeMap as part of PR #3978
    /// - to avoid [`ReadStateService`] response inconsistencies when a block has just been finalized,
    ///   ignore UTXOs that are at a height less than or equal to the finalized tip.
    ///
    /// Optional:
    /// - use Arc<Utxo> to save 2-100 bytes per output?
    /// - if we add an OutputLocation to UTXO, remove this OutputLocation,
    ///   and use the inner OutputLocation to sort Utxos in chain order?
    created_utxos: HashMap<OutputLocation, transparent::Utxo>,

    /// The partial list of UTXOs spent by a transparent address.
    ///
    /// The `getaddressutxos` RPC doesn't need these transaction IDs to be sorted in chain order,
    /// but it might in future. So Zebra does it anyway.
    ///
    /// TODO:
    /// - use BTreeSet as part of PR #3978
    /// - to avoid [`ReadStateService`] response inconsistencies when a block has just been finalized,
    ///   ignore UTXOs that are at a height less than or equal to the finalized tip.
    ///
    /// Optional:
    /// - use Arc<Utxo> to save 2-100 bytes per output
    /// - if we add an OutputLocation to UTXO, remove this OutputLocation,
    ///   and use the inner OutputLocation to sort Utxos in chain order
    spent_utxos: HashSet<OutputLocation>,
}

// A created UTXO
//
// TODO: replace arguments with an Update/Revert enum?
impl
    UpdateWith<(
        // The location of the UTXO
        &transparent::OutPoint,
        // The UTXO data
        &transparent::Utxo,
        // The location of the transaction that creates the UTXO
        &TransactionLocation,
    )> for TransparentTransfers
{
    fn update_chain_tip_with(
        &mut self,
        &(outpoint, utxo, transaction_location): &(
            &transparent::OutPoint,
            &transparent::Utxo,
            &TransactionLocation,
        ),
    ) -> Result<(), ValidateContextError> {
        self.balance = (self.balance + utxo.output.value().constrain().unwrap()).unwrap();

        // TODO: stop creating duplicate UTXOs in the tests, then assert that inserts are unique
        let output_location = OutputLocation::from_outpoint(*transaction_location, outpoint);
        self.created_utxos.insert(output_location, utxo.clone());

        // TODO: store TransactionLocation as part of PR #3978
        self.tx_ids.insert(outpoint.hash);

        Ok(())
    }

    fn revert_chain_with(
        &mut self,
        &(outpoint, utxo, transaction_location): &(
            &transparent::OutPoint,
            &transparent::Utxo,
            &TransactionLocation,
        ),
        _position: RevertPosition,
    ) {
        self.balance = (self.balance - utxo.output.value().constrain().unwrap()).unwrap();

        // TODO: stop creating duplicate UTXOs in the tests, then assert that removed values are present
        let output_location = OutputLocation::from_outpoint(*transaction_location, outpoint);
        self.created_utxos.remove(&output_location);

        assert!(
            self.tx_ids.remove(&outpoint.hash),
            "unexpected created UTXO transaction hash: duplicate revert, or revert that was never updated",
        );
    }
}

// A transparent input
//
// TODO: replace arguments with an Update/Revert enum?
impl
    UpdateWith<(
        // The transparent input data
        &transparent::Input,
        // The transaction the input is from
        &transaction::Hash,
        // The output spent by the input
        &transparent::Output,
        // The location of the transaction that created that output
        &TransactionLocation,
    )> for TransparentTransfers
{
    fn update_chain_tip_with(
        &mut self,
        &(spending_input, spending_tx, spent_output, spent_output_tx_loc): &(
            &transparent::Input,
            &transaction::Hash,
            &transparent::Output,
            &TransactionLocation,
        ),
    ) -> Result<(), ValidateContextError> {
        // Spending a UTXO subtracts value from the balance
        self.balance = (self.balance - spent_output.value().constrain().unwrap()).unwrap();

        let spent_outpoint = spending_input.outpoint().expect("checked by caller");

        let output_location = OutputLocation::from_outpoint(*spent_output_tx_loc, &spent_outpoint);
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
        &(spending_input, spending_tx, spent_output, spent_output_tx_loc): &(
            &transparent::Input,
            &transaction::Hash,
            &transparent::Output,
            &TransactionLocation,
        ),
        _position: RevertPosition,
    ) {
        self.balance = (self.balance + spent_output.value().constrain().unwrap()).unwrap();

        let spent_outpoint = spending_input.outpoint().expect("checked by caller");

        let output_location = OutputLocation::from_outpoint(*spent_output_tx_loc, &spent_outpoint);
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

    /// Returns the partial balance for this address.
    #[allow(dead_code)]
    pub fn balance(&self) -> Amount<NegativeAllowed> {
        self.balance
    }

    /// Returns the [`transaction::Hash`]es that sent or received transparent tranfers to this address,
    /// in this partial chain, TODO: in chain order.
    ///
    /// TODO:
    /// - use (TransactionLocation, transaction::Hash) as part of PR #3978
    /// - return as a BTreeMap<TransactionLocation, transaction::Hash>, so transactions are in chain order
    #[allow(dead_code)]
    pub fn tx_ids(&self) -> HashSet<transaction::Hash> {
        self.tx_ids.distinct_elements().copied().collect()
    }

    /// Returns the unspent transparent outputs sent to this address,
    /// in this partial chain, TODO: in chain order.
    ///
    /// TODO:
    /// - use BTreeMap as part of PR #3978
    #[allow(dead_code)]
    pub fn created_utxos(&self) -> &HashMap<OutputLocation, transparent::Utxo> {
        &self.created_utxos
    }

    /// Returns the spent transparent outputs sent to this address,
    /// in this partial chain, TODO: in chain order.
    ///
    /// TODO:
    /// - use BTreeMap as part of PR #3978
    #[allow(dead_code)]
    pub fn spent_utxos(&self) -> &HashSet<OutputLocation> {
        &self.spent_utxos
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
