use std::convert::TryFrom;

use zebra_chain::{
    amount::Amount,
    primitives::{
        ed25519,
        redjubjub::{self, Binding},
        Groth16Proof,
    },
    sapling::ValueCommitment,
    transaction::{JoinSplitData, ShieldedData},
};

use crate::transaction::VerifyTransactionError;

/// Validate the JoinSplit binding signature.
///
/// https://zips.z.cash/protocol/canopy.pdf#sproutnonmalleability
/// https://zips.z.cash/protocol/canopy.pdf#txnencodingandconsensus
pub fn validate_joinsplit_sig(
    joinsplit_data: JoinSplitData<Groth16Proof>,
    sighash: &[u8],
) -> Result<(), VerifyTransactionError> {
    ed25519::VerificationKey::try_from(joinsplit_data.pub_key)
        .and_then(|vk| vk.verify(&joinsplit_data.sig, sighash))
        .map_err(VerifyTransactionError::Ed25519)
}

/// Calculate the Spend/Output binding signature validating key.
///
/// Getting the binding signature validating key from the Spend and Output
/// description value commitments and the balancing value implicitly checks
/// that the balancing value is consistent with the value transfered in the
/// Spend and Output descriptions but also proves that the signer knew the
/// randomness used for the Spend and Output value commitments, which
/// prevents replays of Output descriptions.
///
/// The net value of Spend transfers minus Output transfers in a transaction
/// is called the balancing value, measured in zatoshi as a signed integer
/// v_balance.
///
/// Consistency of v_balance with the value commitments in Spend
/// descriptions and Output descriptions is enforced by the binding
/// signature.
///
/// Instead of generating a key pair at random, we generate it as a function
/// of the value commitments in the Spend descriptions and Output
/// descriptions of the transaction, and the balancing value.
///
/// https://zips.z.cash/protocol/canopy.pdf#saplingbalance
pub fn balancing_value_balances(
    shielded_data: ShieldedData,
    value_balance: Amount,
) -> Result<redjubjub::VerificationKey<Binding>, redjubjub::Error> {
    let cv_old: ValueCommitment = shielded_data.spends().map(|spend| spend.cv).sum();
    let cv_new: ValueCommitment = shielded_data.outputs().map(|output| output.cv).sum();
    let cv_balance: ValueCommitment = ValueCommitment::new(jubjub::Fr::zero(), value_balance);

    let key_bytes: [u8; 32] = (cv_old - cv_new - cv_balance).into();

    redjubjub::VerificationKey::<Binding>::try_from(key_bytes)
}
