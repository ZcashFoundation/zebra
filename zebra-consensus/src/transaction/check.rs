use std::convert::{TryFrom, TryInto};

use zebra_chain::{
    amount::{self, Amount, NegativeAllowed},
    primitives::{
        ed25519,
        redjubjub::{self, Binding},
        Groth16Proof,
    },
    sapling::ValueCommitment,
    transaction::{JoinSplitData, ShieldedData, Transaction},
};

use crate::transaction::VerifyTransactionError;

/// Validate the JoinSplit binding signature.
///
/// https://zips.z.cash/protocol/canopy.pdf#sproutnonmalleability
/// https://zips.z.cash/protocol/canopy.pdf#txnencodingandconsensus
pub fn validate_joinsplit_sig(
    joinsplit_data: &JoinSplitData<Groth16Proof>,
    sighash: &[u8],
) -> Result<(), VerifyTransactionError> {
    ed25519::VerificationKey::try_from(joinsplit_data.pub_key)
        .and_then(|vk| vk.verify(&joinsplit_data.sig, sighash))
        .map_err(VerifyTransactionError::Ed25519)
}

/// Check that at least one of tx_in_count, nShieldedSpend, and nJoinSplit MUST
/// be non-zero.
///
/// https://zips.z.cash/protocol/canopy.pdf#txnencodingandconsensus
pub fn some_money_is_spent(tx: &Transaction) -> Result<(), VerifyTransactionError> {
    match tx {
        Transaction::V4 {
            inputs,
            joinsplit_data: Some(joinsplit_data),
            shielded_data: Some(shielded_data),
            ..
        } => {
            if inputs.len() > 0
                || joinsplit_data.joinsplits().count() > 0
                || shielded_data.spends().count() > 0
            {
                return Ok(());
            } else {
                return Err(VerifyTransactionError::NoTransfer);
            }
        }
        _ => return Err(VerifyTransactionError::WrongVersion),
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
) -> Result<(), VerifyTransactionError> {
    match tx {
        Transaction::V4 {
            inputs, outputs, ..
        } => {
            if !tx.contains_coinbase_input() {
                return Ok(());
            } else if outputs.len() == 0 {
                return Ok(());
            } else {
                return Err(VerifyTransactionError::NoTransfer);
            }
        }
        _ => return Err(VerifyTransactionError::WrongVersion),
    }
}

/// Check that if there are no Spends or Outputs, that valueBalance is also 0.
///
/// https://zips.z.cash/protocol/canopy.pdf#consensusfrombitcoin
pub fn shielded_balances_match(
    shielded_data: &ShieldedData,
    value_balance: Amount,
) -> Result<(), VerifyTransactionError> {
    if shielded_data.spends().count() + shielded_data.outputs().count() != 0 {
        return Ok(());
    } else if i64::from(value_balance) == 0 {
        return Ok(());
    } else {
        return Err(VerifyTransactionError::BadBalance);
    }
}

/// Check that a coinbase tx does not have any JoinSplit or Spend descriptions.
///
/// https://zips.z.cash/protocol/canopy.pdf#consensusfrombitcoin
pub fn coinbase_tx_does_not_spend_shielded(tx: &Transaction) -> Result<(), VerifyTransactionError> {
    match tx {
        Transaction::V4 {
            joinsplit_data: Some(joinsplit_data),
            shielded_data: Some(shielded_data),
            ..
        } => {
            if !tx.is_coinbase() {
                return Ok(());
            } else if joinsplit_data.joinsplits().count() == 0
                && shielded_data.spends().count() == 0
            {
                return Ok(());
            } else {
                return Err(VerifyTransactionError::Coinbase);
            }
        }
        _ => return Err(VerifyTransactionError::WrongVersion),
    }
}
