//! Transaction checks.
//!
//! Code in this file can freely assume that no pre-V4 transactions are present.

use std::convert::TryFrom;

use zebra_chain::{
    amount::Amount,
    primitives::{ed25519, Groth16Proof},
    transaction::{JoinSplitData, ShieldedData, Transaction},
};

use crate::error::TransactionError;

/// Validate the JoinSplit binding signature.
///
/// https://zips.z.cash/protocol/canopy.pdf#sproutnonmalleability
/// https://zips.z.cash/protocol/canopy.pdf#txnencodingandconsensus
pub fn validate_joinsplit_sig(
    joinsplit_data: &JoinSplitData<Groth16Proof>,
    sighash: &[u8],
) -> Result<(), TransactionError> {
    ed25519::VerificationKey::try_from(joinsplit_data.pub_key)
        .and_then(|vk| vk.verify(&joinsplit_data.sig, sighash))
        .map_err(TransactionError::Ed25519)
}

/// Check that at least one of tx_in_count, nShieldedSpend, and nJoinSplit MUST
/// be non-zero.
///
/// https://zips.z.cash/protocol/canopy.pdf#txnencodingandconsensus
pub fn some_money_is_spent(tx: &Transaction) -> Result<(), TransactionError> {
    match tx {
        Transaction::V4 {
            inputs,
            joinsplit_data: Some(joinsplit_data),
            shielded_data: Some(shielded_data),
            ..
        } => {
            if !inputs.is_empty()
                || joinsplit_data.joinsplits().count() > 0
                || shielded_data.spends().count() > 0
            {
                Ok(())
            } else {
                Err(TransactionError::NoTransfer)
            }
        }
        _ => Err(TransactionError::WrongVersion),
    }
}

/// Check that a transaction with one or more transparent inputs from coinbase
/// transactions has no transparent outputs.
///
/// Note that inputs from coinbase transactions include Founders’ Reward
/// outputs.
///
/// https://zips.z.cash/protocol/canopy.pdf#consensusfrombitcoin
pub fn any_coinbase_inputs_no_transparent_outputs(
    tx: &Transaction,
) -> Result<(), TransactionError> {
    match tx {
        Transaction::V4 { outputs, .. } => {
            if !tx.contains_coinbase_input() || !outputs.is_empty() {
                Ok(())
            } else {
                Err(TransactionError::NoTransfer)
            }
        }
        _ => Err(TransactionError::WrongVersion),
    }
}

/// Check that if there are no Spends or Outputs, that valueBalance is also 0.
///
/// https://zips.z.cash/protocol/canopy.pdf#consensusfrombitcoin
pub fn shielded_balances_match(
    shielded_data: &ShieldedData,
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
/// https://zips.z.cash/protocol/canopy.pdf#txnencodingandconsensus
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
                shielded_data: Some(shielded_data),
                ..
            } if shielded_data.spends().count() > 0 => Err(TransactionError::CoinbaseHasSpend),

            Transaction::V4 { .. } => Ok(()),

            Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
                unreachable!("tx version is checked first")
            }
        }
    } else {
        Ok(())
    }
}
