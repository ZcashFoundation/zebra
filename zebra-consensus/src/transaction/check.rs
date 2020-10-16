use std::convert::TryFrom;

use zebra_chain::{
    amount::Amount,
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
