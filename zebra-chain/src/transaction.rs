//! Transactions and transaction-related structures.

use serde::{Deserialize, Serialize};

mod hash;
mod joinsplit;
mod lock_time;
mod memo;
mod serialize;
mod sighash;
mod txid;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;
#[cfg(test)]
mod tests;

pub use hash::Hash;
pub use joinsplit::JoinSplitData;
pub use lock_time::LockTime;
pub use memo::Memo;
pub use sapling::FieldNotPresent;
pub use sighash::HashType;
pub use sighash::SigHash;

use crate::{
    amount::{Amount, Error as AmountError, NegativeAllowed, NonNegative},
    block, orchard,
    parameters::NetworkUpgrade,
    primitives::{Bctv14Proof, Groth16Proof},
    sapling, sprout, transparent,
    value_balance::ValueBalance,
};

use std::collections::HashMap;

/// A Zcash transaction.
///
/// A transaction is an encoded data structure that facilitates the transfer of
/// value between two public key addresses on the Zcash ecosystem. Everything is
/// designed to ensure that transactions can created, propagated on the network,
/// validated, and finally added to the global ledger of transactions (the
/// blockchain).
///
/// Zcash has a number of different transaction formats. They are represented
/// internally by different enum variants. Because we checkpoint on Canopy
/// activation, we do not validate any pre-Sapling transaction types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// XXX consider boxing the Optional fields of V4 and V5 txs
#[allow(clippy::large_enum_variant)]
pub enum Transaction {
    /// A fully transparent transaction (`version = 1`).
    V1 {
        /// The transparent inputs to the transaction.
        inputs: Vec<transparent::Input>,
        /// The transparent outputs from the transaction.
        outputs: Vec<transparent::Output>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
    },
    /// A Sprout transaction (`version = 2`).
    V2 {
        /// The transparent inputs to the transaction.
        inputs: Vec<transparent::Input>,
        /// The transparent outputs from the transaction.
        outputs: Vec<transparent::Output>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Bctv14Proof>>,
    },
    /// An Overwinter transaction (`version = 3`).
    V3 {
        /// The transparent inputs to the transaction.
        inputs: Vec<transparent::Input>,
        /// The transparent outputs from the transaction.
        outputs: Vec<transparent::Output>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The latest block height that this transaction can be added to the chain.
        expiry_height: block::Height,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Bctv14Proof>>,
    },
    /// A Sapling transaction (`version = 4`).
    V4 {
        /// The transparent inputs to the transaction.
        inputs: Vec<transparent::Input>,
        /// The transparent outputs from the transaction.
        outputs: Vec<transparent::Output>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The latest block height that this transaction can be added to the chain.
        expiry_height: block::Height,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
        /// The sapling shielded data for this transaction, if any.
        sapling_shielded_data: Option<sapling::ShieldedData<sapling::PerSpendAnchor>>,
    },
    /// A `version = 5` transaction, which supports `Sapling` and `Orchard`.
    V5 {
        /// The Network Upgrade for this transaction.
        ///
        /// Derived from the ConsensusBranchId field.
        network_upgrade: NetworkUpgrade,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The latest block height that this transaction can be added to the chain.
        expiry_height: block::Height,
        /// The transparent inputs to the transaction.
        inputs: Vec<transparent::Input>,
        /// The transparent outputs from the transaction.
        outputs: Vec<transparent::Output>,
        /// The sapling shielded data for this transaction, if any.
        sapling_shielded_data: Option<sapling::ShieldedData<sapling::SharedAnchor>>,
        /// The orchard data for this transaction, if any.
        orchard_shielded_data: Option<orchard::ShieldedData>,
    },
}

impl Transaction {
    // hashes

    /// Compute the hash (id) of this transaction.
    pub fn hash(&self) -> Hash {
        Hash::from(self)
    }

    /// Calculate the sighash for the current transaction
    ///
    /// # Details
    ///
    /// The `input` argument indicates the transparent Input for which we are
    /// producing a sighash. It is comprised of the index identifying the
    /// transparent::Input within the transaction and the transparent::Output
    /// representing the UTXO being spent by that input.
    ///
    /// # Panics
    ///
    /// - if passed in any NetworkUpgrade from before NetworkUpgrade::Overwinter
    /// - if called on a v1 or v2 transaction
    /// - if the input index points to a transparent::Input::CoinBase
    /// - if the input index is out of bounds for self.inputs()
    pub fn sighash(
        &self,
        network_upgrade: NetworkUpgrade,
        hash_type: sighash::HashType,
        input: Option<(u32, transparent::Output)>,
    ) -> SigHash {
        sighash::SigHasher::new(self, hash_type, network_upgrade, input).sighash()
    }

    // header

    /// Return if the `fOverwintered` flag of this transaction is set.
    pub fn is_overwintered(&self) -> bool {
        match self {
            Transaction::V1 { .. } | Transaction::V2 { .. } => false,
            Transaction::V3 { .. } | Transaction::V4 { .. } | Transaction::V5 { .. } => true,
        }
    }

    /// Return the version of this transaction.
    pub fn version(&self) -> u32 {
        match self {
            Transaction::V1 { .. } => 1,
            Transaction::V2 { .. } => 2,
            Transaction::V3 { .. } => 3,
            Transaction::V4 { .. } => 4,
            Transaction::V5 { .. } => 5,
        }
    }

    /// Get this transaction's lock time.
    pub fn lock_time(&self) -> LockTime {
        match self {
            Transaction::V1 { lock_time, .. } => *lock_time,
            Transaction::V2 { lock_time, .. } => *lock_time,
            Transaction::V3 { lock_time, .. } => *lock_time,
            Transaction::V4 { lock_time, .. } => *lock_time,
            Transaction::V5 { lock_time, .. } => *lock_time,
        }
    }

    /// Get this transaction's expiry height, if any.
    pub fn expiry_height(&self) -> Option<block::Height> {
        match self {
            Transaction::V1 { .. } => None,
            Transaction::V2 { .. } => None,
            Transaction::V3 { expiry_height, .. } => Some(*expiry_height),
            Transaction::V4 { expiry_height, .. } => Some(*expiry_height),
            Transaction::V5 { expiry_height, .. } => Some(*expiry_height),
        }
    }

    /// Get this transaction's network upgrade field, if any.
    /// This field is serialized as `nConsensusBranchId` ([7.1]).
    ///
    /// [7.1]: https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
    pub fn network_upgrade(&self) -> Option<NetworkUpgrade> {
        match self {
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => None,
            Transaction::V5 {
                network_upgrade, ..
            } => Some(*network_upgrade),
        }
    }

    // transparent

    /// Access the transparent inputs of this transaction, regardless of version.
    pub fn inputs(&self) -> &[transparent::Input] {
        match self {
            Transaction::V1 { ref inputs, .. } => inputs,
            Transaction::V2 { ref inputs, .. } => inputs,
            Transaction::V3 { ref inputs, .. } => inputs,
            Transaction::V4 { ref inputs, .. } => inputs,
            Transaction::V5 { ref inputs, .. } => inputs,
        }
    }

    /// Modify the transparent inputs of this transaction, regardless of version.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn inputs_mut(&mut self) -> &mut Vec<transparent::Input> {
        match self {
            Transaction::V1 { ref mut inputs, .. } => inputs,
            Transaction::V2 { ref mut inputs, .. } => inputs,
            Transaction::V3 { ref mut inputs, .. } => inputs,
            Transaction::V4 { ref mut inputs, .. } => inputs,
            Transaction::V5 { ref mut inputs, .. } => inputs,
        }
    }

    /// Access the transparent outputs of this transaction, regardless of version.
    pub fn outputs(&self) -> &[transparent::Output] {
        match self {
            Transaction::V1 { ref outputs, .. } => outputs,
            Transaction::V2 { ref outputs, .. } => outputs,
            Transaction::V3 { ref outputs, .. } => outputs,
            Transaction::V4 { ref outputs, .. } => outputs,
            Transaction::V5 { ref outputs, .. } => outputs,
        }
    }

    /// Returns `true` if this transaction is a coinbase transaction.
    pub fn is_coinbase(&self) -> bool {
        self.inputs().len() == 1
            && matches!(
                self.inputs().get(0),
                Some(transparent::Input::Coinbase { .. })
            )
    }

    /// Returns `true` if transaction contains any coinbase inputs.
    pub fn contains_coinbase_input(&self) -> bool {
        self.inputs()
            .iter()
            .any(|input| matches!(input, transparent::Input::Coinbase { .. }))
    }

    /// Returns `true` if transaction contains any `PrevOut` inputs.
    ///
    /// `PrevOut` inputs are also known as `transparent` inputs in the spec.
    pub fn contains_prevout_input(&self) -> bool {
        self.inputs()
            .iter()
            .any(|input| matches!(input, transparent::Input::PrevOut { .. }))
    }

    // sprout

    /// Returns the number of `JoinSplit`s in this transaction, regardless of version.
    pub fn joinsplit_count(&self) -> usize {
        match self {
            // JoinSplits with Bctv14 Proofs
            Transaction::V2 {
                joinsplit_data: Some(joinsplit_data),
                ..
            }
            | Transaction::V3 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.joinsplits().count(),
            // JoinSplits with Groth Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.joinsplits().count(),
            // No JoinSplits
            Transaction::V1 { .. }
            | Transaction::V2 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V3 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V4 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V5 { .. } => 0,
        }
    }

    /// Returns the `vpub_old` fields from `JoinSplit`s in this transaction, regardless of version.
    ///
    /// This value is removed from the transparent value pool of this transaction, and added to the
    /// sprout value pool.
    pub fn sprout_pool_added_values(&self) -> Box<dyn Iterator<Item = &Amount<NonNegative>> + '_> {
        match self {
            // JoinSplits with Bctv14 Proofs
            Transaction::V2 {
                joinsplit_data: Some(joinsplit_data),
                ..
            }
            | Transaction::V3 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(
                joinsplit_data
                    .joinsplits()
                    .map(|joinsplit| &joinsplit.vpub_old),
            ),
            // JoinSplits with Groth Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(
                joinsplit_data
                    .joinsplits()
                    .map(|joinsplit| &joinsplit.vpub_old),
            ),
            // No JoinSplits
            Transaction::V1 { .. }
            | Transaction::V2 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V3 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V4 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V5 { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Access the sprout::Nullifiers in this transaction, regardless of version.
    pub fn sprout_nullifiers(&self) -> Box<dyn Iterator<Item = &sprout::Nullifier> + '_> {
        // This function returns a boxed iterator because the different
        // transaction variants end up having different iterator types
        // (we could extract bctv and groth as separate iterators, then chain
        // them together, but that would be much harder to read and maintain)
        match self {
            // JoinSplits with Bctv14 Proofs
            Transaction::V2 {
                joinsplit_data: Some(joinsplit_data),
                ..
            }
            | Transaction::V3 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(joinsplit_data.nullifiers()),
            // JoinSplits with Groth Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(joinsplit_data.nullifiers()),
            // No JoinSplits
            Transaction::V1 { .. }
            | Transaction::V2 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V3 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V4 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V5 { .. } => Box::new(std::iter::empty()),
        }
    }

    // sapling

    /// Iterate over the sapling [`Spend`](sapling::Spend)s for this transaction,
    /// returning `Spend<PerSpendAnchor>` regardless of the underlying
    /// transaction version.
    ///
    /// Shared anchors in V5 transactions are copied into each sapling spend.
    /// This allows the same code to validate spends from V4 and V5 transactions.
    ///
    /// # Correctness
    ///
    /// Do not use this function for serialization.
    pub fn sapling_spends_per_anchor(
        &self,
    ) -> Box<dyn Iterator<Item = sapling::Spend<sapling::PerSpendAnchor>> + '_> {
        match self {
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.spends_per_anchor()),
            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.spends_per_anchor()),

            // No Spends
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 {
                sapling_shielded_data: None,
                ..
            }
            | Transaction::V5 {
                sapling_shielded_data: None,
                ..
            } => Box::new(std::iter::empty()),
        }
    }

    /// Iterate over the sapling [`Output`](sapling::Output)s for this
    /// transaction
    pub fn sapling_outputs(&self) -> Box<dyn Iterator<Item = &sapling::Output> + '_> {
        match self {
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.outputs()),
            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.outputs()),

            // No Outputs
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 {
                sapling_shielded_data: None,
                ..
            }
            | Transaction::V5 {
                sapling_shielded_data: None,
                ..
            } => Box::new(std::iter::empty()),
        }
    }

    /// Access the sapling::Nullifiers in this transaction, regardless of version.
    pub fn sapling_nullifiers(&self) -> Box<dyn Iterator<Item = &sapling::Nullifier> + '_> {
        // This function returns a boxed iterator because the different
        // transaction variants end up having different iterator types
        match self {
            // Spends with Groth Proofs
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.nullifiers()),
            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.nullifiers()),

            // No Spends
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 {
                sapling_shielded_data: None,
                ..
            }
            | Transaction::V5 {
                sapling_shielded_data: None,
                ..
            } => Box::new(std::iter::empty()),
        }
    }

    /// Return if the transaction has any Sapling shielded data.
    pub fn has_sapling_shielded_data(&self) -> bool {
        match self {
            Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => false,
            Transaction::V4 {
                sapling_shielded_data,
                ..
            } => sapling_shielded_data.is_some(),
            Transaction::V5 {
                sapling_shielded_data,
                ..
            } => sapling_shielded_data.is_some(),
        }
    }

    // orchard

    /// Access the [`orchard::ShieldedData`] in this transaction, if there are any,
    /// regardless of version.
    pub fn orchard_shielded_data(&self) -> Option<&orchard::ShieldedData> {
        match self {
            // Maybe Orchard shielded data
            Transaction::V5 {
                orchard_shielded_data,
                ..
            } => orchard_shielded_data.as_ref(),

            // No Orchard shielded data
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => None,
        }
    }

    /// Iterate over the [`orchard::Action`]s in this transaction, if there are any,
    /// regardless of version.
    pub fn orchard_actions(&self) -> impl Iterator<Item = &orchard::Action> {
        self.orchard_shielded_data()
            .into_iter()
            .map(orchard::ShieldedData::actions)
            .flatten()
    }

    /// Access the [`orchard::Nullifier`]s in this transaction, if there are any,
    /// regardless of version.
    pub fn orchard_nullifiers(&self) -> impl Iterator<Item = &orchard::Nullifier> {
        self.orchard_shielded_data()
            .into_iter()
            .map(orchard::ShieldedData::nullifiers)
            .flatten()
    }

    /// Access the [`orchard::Flags`] in this transaction, if there is any,
    /// regardless of version.
    pub fn orchard_flags(&self) -> Option<orchard::shielded_data::Flags> {
        self.orchard_shielded_data()
            .map(|orchard_shielded_data| orchard_shielded_data.flags)
    }

    // value pool

    /// Get all the value pools for this transaction
    pub fn value_balance(
        &self,
        utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    ) -> Result<ValueBalance<NegativeAllowed>, Box<dyn std::error::Error>> {
        match self {
            Transaction::V1 {
                inputs, outputs, ..
            } => {
                let transparent = transparent_value_pool(inputs, outputs, utxos);

                let mut value_balance = ValueBalance::zero();
                value_balance.set_transparent_value_balance(transparent?);
                Ok(value_balance)
            }
            Transaction::V2 {
                inputs,
                outputs,
                joinsplit_data,
                ..
            } => {
                let transparent = transparent_value_pool(inputs, outputs, utxos);
                let sprout = sprout_value_pool(joinsplit_data);

                let mut value_balance = ValueBalance::zero();
                value_balance.set_transparent_value_balance(transparent?);
                value_balance.set_sprout_value_balance(sprout?);
                Ok(value_balance)
            }
            Transaction::V3 {
                inputs,
                outputs,
                joinsplit_data,
                ..
            } => {
                let transparent = transparent_value_pool(inputs, outputs, utxos);
                let sprout = sprout_value_pool(joinsplit_data);

                let mut value_balance = ValueBalance::zero();
                value_balance.set_transparent_value_balance(transparent?);
                value_balance.set_sprout_value_balance(sprout?);
                Ok(value_balance)
            }
            Transaction::V4 {
                inputs,
                outputs,
                joinsplit_data,
                sapling_shielded_data,
                ..
            } => {
                let transparent = transparent_value_pool(inputs, outputs, utxos);
                let sprout = sprout_value_pool(joinsplit_data);
                let sapling = sapling_value_pool(sapling_shielded_data);

                let mut value_balance = ValueBalance::zero();
                value_balance.set_transparent_value_balance(transparent?);
                value_balance.set_sprout_value_balance(sprout?);
                value_balance.set_sapling_value_balance(sapling?);
                Ok(value_balance)
            }
            Transaction::V5 {
                inputs,
                outputs,
                sapling_shielded_data,
                orchard_shielded_data,
                ..
            } => {
                let transparent = transparent_value_pool(inputs, outputs, utxos);
                let sapling = sapling_value_pool(sapling_shielded_data);
                let orchard = orchard_value_pool(orchard_shielded_data);

                let mut value_balance = ValueBalance::zero();
                value_balance.set_transparent_value_balance(transparent?);
                value_balance.set_sapling_value_balance(sapling?);
                value_balance.set_orchard_value_balance(orchard?);
                Ok(value_balance)
            }
        }
    }
}

// Value pool utility functions

/// Return the transparent value pool
fn transparent_value_pool(
    inputs: &[transparent::Input],
    outputs: &[transparent::Output],
    utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
) -> Result<ValueBalance<NegativeAllowed>, Box<dyn std::error::Error>> {
    if inputs.is_empty() && outputs.is_empty() {
        return Ok(ValueBalance::zero());
    }

    let input_value_balance: Amount = inputs
        .iter()
        .flat_map(|i| i.value_balance(utxos))
        .sum::<Result<Amount, AmountError>>()?;

    let output_value_balance: Amount<NegativeAllowed> = outputs
        .iter()
        .map(|o| o.value_balance())
        .sum::<Result<Amount, AmountError>>()?;

    Ok(ValueBalance::from_transparent_amount(
        (input_value_balance - output_value_balance)?,
    ))
}

use crate::primitives::ZkSnarkProof;
fn sprout_value_pool<P: ZkSnarkProof>(
    joinsplit_data: &Option<JoinSplitData<P>>,
) -> Result<ValueBalance<NegativeAllowed>, Box<dyn std::error::Error>> {
    let sprout = joinsplit_data
        .iter()
        .flat_map(|j| j.value_balance())
        .sum::<Result<Amount, AmountError>>()?;

    Ok(ValueBalance::from_sprout_amount(sprout))
}

fn sapling_value_pool<P: sapling::AnchorVariant + Clone>(
    sapling_shielded_data: &Option<sapling::ShieldedData<P>>,
) -> Result<ValueBalance<NegativeAllowed>, Box<dyn std::error::Error>> {
    let sapling = sapling_shielded_data
        .iter()
        .map(|s| s.value_balance())
        .sum::<Result<Amount, AmountError>>()?;

    Ok(ValueBalance::from_sapling_amount(sapling))
}

fn orchard_value_pool(
    orchard_shielded_data: &Option<orchard::ShieldedData>,
) -> Result<ValueBalance<NegativeAllowed>, Box<dyn std::error::Error>> {
    let orchard = orchard_shielded_data
        .iter()
        .map(|o| o.value_balance())
        .sum::<Result<Amount, AmountError>>()?;

    Ok(ValueBalance::from_orchard_amount(orchard))
}
