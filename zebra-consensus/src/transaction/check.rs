//! Transaction checks.
//!
//! Code in this file can freely assume that no pre-V4 transactions are present.

use zebra_chain::{
    orchard::Flags,
    sapling::{Output, PerSpendAnchor, Spend},
    transaction::Transaction,
};

use crate::error::TransactionError;

/// Checks that the transaction has inputs and outputs.
///
/// For `Transaction::V4`:
/// * At least one of `tx_in_count`, `nSpendsSapling`, and `nJoinSplit` MUST be non-zero.
/// * At least one of `tx_out_count`, `nOutputsSapling`, and `nJoinSplit` MUST be non-zero.
///
/// For `Transaction::V5`:
/// * This condition must hold: `tx_in_count` > 0 or `nSpendsSapling` > 0 or
/// (`nActionsOrchard` > 0 and `enableSpendsOrchard` = 1)
/// * This condition must hold: `tx_out_count` > 0 or `nOutputsSapling` > 0 or
/// (`nActionsOrchard` > 0 and `enableOutputsOrchard` = 1)
///
/// This check counts both `Coinbase` and `PrevOut` transparent inputs.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn has_inputs_and_outputs(tx: &Transaction) -> Result<(), TransactionError> {
    let tx_in_count = tx.inputs().len();
    let tx_out_count = tx.outputs().len();
    let n_joinsplit = tx.joinsplit_count();
    let n_spends_sapling = tx.sapling_spends_per_anchor().count();
    let n_outputs_sapling = tx.sapling_outputs().count();
    let n_actions_orchard = tx.orchard_actions().count();
    let flags_orchard = tx.orchard_flags().unwrap_or_else(Flags::empty);

    // TODO: Improve the code to express the spec rules better #2410.
    if tx_in_count
        + n_spends_sapling
        + n_joinsplit
        + (n_actions_orchard > 0 && flags_orchard.contains(Flags::ENABLE_SPENDS)) as usize
        == 0
    {
        Err(TransactionError::NoInputs)
    } else if tx_out_count
        + n_outputs_sapling
        + n_joinsplit
        + (n_actions_orchard > 0 && flags_orchard.contains(Flags::ENABLE_OUTPUTS)) as usize
        == 0
    {
        Err(TransactionError::NoOutputs)
    } else {
        Ok(())
    }
}

/// Check that a coinbase transaction has no PrevOut inputs, JoinSplits, or spends.
///
/// A coinbase transaction MUST NOT have any transparent inputs, JoinSplit descriptions,
/// or Spend descriptions.
///
/// In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
///
/// This check only counts `PrevOut` transparent inputs.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn coinbase_tx_no_prevout_joinsplit_spend(tx: &Transaction) -> Result<(), TransactionError> {
    if tx.is_coinbase() {
        if tx.contains_prevout_input() {
            return Err(TransactionError::CoinbaseHasPrevOutInput);
        } else if tx.joinsplit_count() > 0 {
            return Err(TransactionError::CoinbaseHasJoinSplit);
        } else if tx.sapling_spends_per_anchor().count() > 0 {
            return Err(TransactionError::CoinbaseHasSpend);
        }

        if let Some(orchard_shielded_data) = tx.orchard_shielded_data() {
            if orchard_shielded_data.flags.contains(Flags::ENABLE_SPENDS) {
                return Err(TransactionError::CoinbaseHasEnableSpendsOrchard);
            }
        }
    }

    Ok(())
}

/// Check that a Spend description's cv and rk are not of small order,
/// i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]rk MUST NOT be 𝒪_J.
///
/// https://zips.z.cash/protocol/protocol.pdf#spenddesc
pub fn spend_cv_rk_not_small_order(spend: &Spend<PerSpendAnchor>) -> Result<(), TransactionError> {
    if bool::from(spend.cv.0.is_small_order())
        || bool::from(
            jubjub::AffinePoint::from_bytes(spend.rk.into())
                .unwrap()
                .is_small_order(),
        )
    {
        Err(TransactionError::SmallOrder)
    } else {
        Ok(())
    }
}

/// Check that a Output description's cv and epk are not of small order,
/// i.e. [h_J]cv MUST NOT be 𝒪_J and [h_J]epk MUST NOT be 𝒪_J.
///
/// https://zips.z.cash/protocol/protocol.pdf#outputdesc
pub fn output_cv_epk_not_small_order(output: &Output) -> Result<(), TransactionError> {
    if bool::from(output.cv.0.is_small_order())
        || bool::from(
            jubjub::AffinePoint::from_bytes(output.ephemeral_key.into())
                .unwrap()
                .is_small_order(),
        )
    {
        Err(TransactionError::SmallOrder)
    } else {
        Ok(())
    }
}
