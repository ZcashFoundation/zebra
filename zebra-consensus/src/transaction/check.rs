//! Transaction checks.
//!
//! Code in this file can freely assume that no pre-V4 transactions are present.

use std::convert::TryFrom;

use zebra_chain::{
    amount::Amount,
    primitives::{ed25519, Groth16Proof},
    sapling::{Output, Spend},
    transaction::{JoinSplitData, Transaction},
};

use crate::error::TransactionError;

/// Validate the JoinSplit binding signature.
///
/// https://zips.z.cash/protocol/protocol.pdf#sproutnonmalleability
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn validate_joinsplit_sig(
    joinsplit_data: &JoinSplitData<Groth16Proof>,
    sighash: &[u8],
) -> Result<(), TransactionError> {
    // TODO: batch verify ed25519: https://github.com/ZcashFoundation/zebra/issues/1944
    ed25519::VerificationKey::try_from(joinsplit_data.pub_key)
        .and_then(|vk| vk.verify(&joinsplit_data.sig, sighash))
        .map_err(TransactionError::Ed25519)
}

/// Checks that the transaction has inputs and outputs.
///
/// More specifically:
///
/// * at least one of tx_in_count, nShieldedSpend, and nJoinSplit MUST be non-zero.
/// * at least one of tx_out_count, nShieldedOutput, and nJoinSplit MUST be non-zero.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn has_inputs_and_outputs(tx: &Transaction) -> Result<(), TransactionError> {
    // The consensus rule is written in terms of numbers, but our transactions
    // hold enum'd data. Mixing pattern matching and numerical checks is risky,
    // so convert everything to counts and sum up.
    match tx {
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
            let n_shielded_spend = sapling_shielded_data
                .as_ref()
                .map(|d| d.spends().count())
                .unwrap_or(0);
            let n_shielded_output = sapling_shielded_data
                .as_ref()
                .map(|d| d.outputs().count())
                .unwrap_or(0);

            if tx_in_count + n_shielded_spend + n_joinsplit == 0 {
                Err(TransactionError::NoInputs)
            } else if tx_out_count + n_shielded_output + n_joinsplit == 0 {
                Err(TransactionError::NoOutputs)
            } else {
                Ok(())
            }
        }
        Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
            unreachable!("tx version is checked first")
        }
        Transaction::V5 { .. } => {
            unimplemented!("v5 transaction format as specified in ZIP-225")
        }
    }
}

/// Check that if there are no Spends or Outputs, that valueBalance is also 0.
///
/// https://zips.z.cash/protocol/protocol.pdf#consensusfrombitcoin
pub fn shielded_balances_match<T: sapling::AnchorVariant>(
    shielded_data: &sapling::ShieldedData<T>,
    value_balance: Amount,
) -> Result<(), TransactionError> {
    if (shielded_data.spends().count() + shielded_data.outputs().count() != 0)
        || i64::from(value_balance) == 0
    {
        Ok(())
    } else {
        Err(TransactionError::BadBalance)
    }
}

/// Check that a coinbase tx does not have any JoinSplit or Spend descriptions.
///
/// https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus
pub fn coinbase_tx_no_joinsplit_or_spend(tx: &Transaction) -> Result<(), TransactionError> {
    if tx.is_coinbase() {
        match tx {
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

            Transaction::V4 { .. } => Ok(()),

            Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                unreachable!("tx version is checked first")
            }

            Transaction::V5 { .. } => {
                unimplemented!("v5 coinbase validation as specified in ZIP-225 and the draft spec")
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
pub fn spend_cv_rk_not_small_order(spend: &Spend) -> Result<(), TransactionError> {
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
