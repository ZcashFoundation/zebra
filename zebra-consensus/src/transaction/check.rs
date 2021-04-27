//! Transaction checks.
//!
//! Code in this file can freely assume that no pre-V4 transactions are present.

use zebra_chain::{
    sapling::{AnchorVariant, Output, PerSpendAnchor, ShieldedData, Spend},
    transaction::Transaction,
};

use crate::error::TransactionError;

/// Checks that the transaction has inputs and outputs.
///
/// For `Transaction::V4`:
/// * at least one of `tx_in_count`, `nSpendsSapling`, and `nJoinSplit` MUST be non-zero.
/// * at least one of `tx_out_count`, `nOutputsSapling`, and `nJoinSplit` MUST be non-zero.
///
/// For `Transaction::V5`:
/// * at least one of `tx_in_count`, `nSpendsSapling`, and `nActionsOrchard` MUST be non-zero.
/// * at least one of `tx_out_count`, `nOutputsSapling`, and `nActionsOrchard` MUST be non-zero.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn has_inputs_and_outputs(tx: &Transaction) -> Result<(), TransactionError> {
    // The consensus rule is written in terms of numbers, but our transactions
    // hold enum'd data. Mixing pattern matching and numerical checks is risky,
    // so convert everything to counts and sum up.
    match tx {
        // In Zebra, `inputs` contains both `PrevOut` (transparent) and `Coinbase` inputs.
        Transaction::V4 {
            inputs,
            outputs,
            joinsplit_data,
            sapling_shielded_data,
            ..
        } => {
            let tx_in_count = inputs.len();
            let tx_out_count = outputs.len();
            let n_joinsplit = joinsplit_data
                .as_ref()
                .map(|d| d.joinsplits().count())
                .unwrap_or(0);
            let n_spends_sapling = sapling_shielded_data
                .as_ref()
                .map(|d| d.spends().count())
                .unwrap_or(0);
            let n_outputs_sapling = sapling_shielded_data
                .as_ref()
                .map(|d| d.outputs().count())
                .unwrap_or(0);

            if tx_in_count + n_spends_sapling + n_joinsplit == 0 {
                Err(TransactionError::NoInputs)
            } else if tx_out_count + n_outputs_sapling + n_joinsplit == 0 {
                Err(TransactionError::NoOutputs)
            } else {
                Ok(())
            }
        }

        Transaction::V5 {
            inputs,
            outputs,
            sapling_shielded_data,
            // TODO: Orchard validation (#1980)
            ..
        } => {
            let tx_in_count = inputs.len();
            let tx_out_count = outputs.len();
            let n_spends_sapling = sapling_shielded_data
                .as_ref()
                .map(|d| d.spends().count())
                .unwrap_or(0);
            let n_outputs_sapling = sapling_shielded_data
                .as_ref()
                .map(|d| d.outputs().count())
                .unwrap_or(0);

            // TODO: Orchard validation (#1980)
            // For `Transaction::V5`:
            // * at least one of `tx_in_count`, `nSpendsSapling`, and `nActionsOrchard` MUST be non-zero.
            // * at least one of `tx_out_count`, `nOutputsSapling`, and `nActionsOrchard` MUST be non-zero.
            if tx_in_count + n_spends_sapling == 0 {
                Err(TransactionError::NoInputs)
            } else if tx_out_count + n_outputs_sapling == 0 {
                Err(TransactionError::NoOutputs)
            } else {
                Ok(())
            }
        }

        Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
            unreachable!("tx version is checked first")
        }
    }
}

/// Check that if there are no Spends or Outputs, that valueBalance is also 0.
///
/// https://zips.z.cash/protocol/protocol.pdf#consensusfrombitcoin
pub fn shielded_balances_match<AnchorV>(
    shielded_data: &ShieldedData<AnchorV>,
) -> Result<(), TransactionError>
where
    AnchorV: AnchorVariant + Clone,
{
    if (shielded_data.spends().count() + shielded_data.outputs().count() != 0)
        || i64::from(shielded_data.value_balance) == 0
    {
        Ok(())
    } else {
        Err(TransactionError::BadBalance)
    }
}

/// Check that a coinbase transaction has no PrevOut inputs, JoinSplits, or spends.
///
/// A coinbase transaction MUST NOT have any transparent inputs, JoinSplit descriptions,
/// or Spend descriptions.
///
/// In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn coinbase_tx_no_prevout_joinsplit_spend(tx: &Transaction) -> Result<(), TransactionError> {
    if tx.is_coinbase() {
        match tx {
            // In Zebra, `inputs` contains both `PrevOut` (transparent) and `Coinbase` inputs.
            tx if tx.contains_prevout_input() => Err(TransactionError::CoinbaseHasPrevOutInput),

            // Check if there is any JoinSplitData.
            Transaction::V4 {
                joinsplit_data: Some(_),
                ..
            } => Err(TransactionError::CoinbaseHasJoinSplit),

            // The ShieldedData contains both Spends and Outputs, and Outputs
            // are allowed post-Heartwood, so we have to count Spends.
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } if sapling_shielded_data.spends().count() > 0 => {
                Err(TransactionError::CoinbaseHasSpend)
            }

            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } if sapling_shielded_data.spends().count() > 0 => {
                Err(TransactionError::CoinbaseHasSpend)
            }

            // TODO: Orchard validation (#1980)
            // In a version 5 coinbase transaction, the enableSpendsOrchard flag MUST be 0.
            Transaction::V4 { .. } | Transaction::V5 { .. } => Ok(()),

            Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                unreachable!("tx version is checked first")
            }
        }
    } else {
        Ok(())
    }
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
