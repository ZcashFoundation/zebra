//! Transactions and transaction-related structures.

use halo2::pasta::pallas;
use serde::{Deserialize, Serialize};

mod auth_digest;
mod hash;
mod joinsplit;
mod lock_time;
mod memo;
mod serialize;
mod sighash;
mod txid;
mod unmined;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;
#[cfg(test)]
mod tests;

pub use auth_digest::AuthDigest;
pub use hash::{Hash, WtxId};
pub use joinsplit::JoinSplitData;
pub use lock_time::LockTime;
pub use memo::Memo;
pub use sapling::FieldNotPresent;
pub use sighash::{HashType, SigHash};
pub use unmined::{UnminedTx, UnminedTxId, VerifiedUnminedTx};

use crate::{
    amount::{Amount, Error as AmountError, NegativeAllowed, NonNegative},
    block, orchard,
    parameters::NetworkUpgrade,
    primitives::{ed25519, Bctv14Proof, Groth16Proof},
    sapling, sprout,
    transparent::{
        self, outputs_from_utxos,
        CoinbaseSpendRestriction::{self, *},
    },
    value_balance::{ValueBalance, ValueBalanceError},
};

use std::{collections::HashMap, fmt, iter};

/// A Zcash transaction.
///
/// A transaction is an encoded data structure that facilitates the transfer of
/// value between two public key addresses on the Zcash ecosystem. Everything is
/// designed to ensure that transactions can be created, propagated on the
/// network, validated, and finally added to the global ledger of transactions
/// (the blockchain).
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
    /// A `version = 5` transaction , which supports Orchard, Sapling, and transparent, but not Sprout.
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

impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct("Transaction");

        fmter.field("version", &self.version());
        if let Some(network_upgrade) = self.network_upgrade() {
            fmter.field("network_upgrade", &network_upgrade);
        }

        fmter.field("transparent_inputs", &self.inputs().len());
        fmter.field("transparent_outputs", &self.outputs().len());
        fmter.field("sprout_joinsplits", &self.joinsplit_count());
        fmter.field("sapling_spends", &self.sapling_spends_per_anchor().count());
        fmter.field("sapling_outputs", &self.sapling_outputs().count());
        fmter.field("orchard_actions", &self.orchard_actions().count());

        fmter.field("unmined_id", &self.unmined_id());

        fmter.finish()
    }
}

impl Transaction {
    // identifiers and hashes

    /// Compute the hash (mined transaction ID) of this transaction.
    ///
    /// The hash uniquely identifies mined v5 transactions,
    /// and all v1-v4 transactions, whether mined or unmined.
    pub fn hash(&self) -> Hash {
        Hash::from(self)
    }

    /// Compute the unmined transaction ID of this transaction.
    ///
    /// This ID uniquely identifies unmined transactions,
    /// regardless of version.
    pub fn unmined_id(&self) -> UnminedTxId {
        UnminedTxId::from(self)
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

    /// Compute the authorizing data commitment of this transaction as specified
    /// in [ZIP-244].
    ///
    /// Returns None for pre-v5 transactions.
    ///
    /// [ZIP-244]: https://zips.z.cash/zip-0244.
    pub fn auth_digest(&self) -> Option<AuthDigest> {
        match self {
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => None,
            Transaction::V5 { .. } => Some(AuthDigest::from(self)),
        }
    }

    // other properties

    /// Does this transaction have transparent or shielded inputs?
    ///
    /// "[Sapling onward] If effectiveVersion < 5, then at least one of tx_in_count,
    /// nSpendsSapling, and nJoinSplit MUST be nonzero.
    ///
    /// [NU5 onward] If effectiveVersion ≥ 5 then this condition MUST hold:
    /// tx_in_count > 0 or nSpendsSapling > 0 or (nActionsOrchard > 0 and enableSpendsOrchard = 1)."
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    pub fn has_transparent_or_shielded_inputs(&self) -> bool {
        !self.inputs().is_empty() || self.has_shielded_inputs()
    }

    /// Does this transaction have shielded inputs?
    ///
    /// See [`has_transparent_or_shielded_inputs`] for details.
    pub fn has_shielded_inputs(&self) -> bool {
        self.joinsplit_count() > 0
            || self.sapling_spends_per_anchor().count() > 0
            || (self.orchard_actions().count() > 0
                && self
                    .orchard_flags()
                    .unwrap_or_else(orchard::Flags::empty)
                    .contains(orchard::Flags::ENABLE_SPENDS))
    }

    /// Does this transaction have transparent or shielded outputs?
    ///
    /// "[Sapling onward] If effectiveVersion < 5, then at least one of tx_out_count,
    /// nOutputsSapling, and nJoinSplit MUST be nonzero.
    ///
    /// [NU5 onward] If effectiveVersion ≥ 5 then this condition MUST hold:
    /// tx_out_count > 0 or nOutputsSapling > 0 or (nActionsOrchard > 0 and enableOutputsOrchard = 1)."
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    pub fn has_transparent_or_shielded_outputs(&self) -> bool {
        !self.outputs().is_empty() || self.has_shielded_outputs()
    }

    /// Does this transaction have shielded outputs?
    ///
    /// See [`has_transparent_or_shielded_outputs`] for details.
    pub fn has_shielded_outputs(&self) -> bool {
        self.joinsplit_count() > 0
            || self.sapling_outputs().count() > 0
            || (self.orchard_actions().count() > 0
                && self
                    .orchard_flags()
                    .unwrap_or_else(orchard::Flags::empty)
                    .contains(orchard::Flags::ENABLE_OUTPUTS))
    }

    /// Does this transaction has at least one flag when we have at least one orchard action?
    ///
    /// [NU5 onward] If effectiveVersion >= 5 and nActionsOrchard > 0, then at least one
    /// of enableSpendsOrchard and enableOutputsOrchard MUST be 1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    pub fn has_enough_orchard_flags(&self) -> bool {
        if self.version() < 5 || self.orchard_actions().count() == 0 {
            return true;
        }
        self.orchard_flags()
            .unwrap_or_else(orchard::Flags::empty)
            .intersects(orchard::Flags::ENABLE_SPENDS | orchard::Flags::ENABLE_OUTPUTS)
    }

    /// Returns the [`CoinbaseSpendRestriction`] for this transaction,
    /// assuming it is mined at `spend_height`.
    pub fn coinbase_spend_restriction(
        &self,
        spend_height: block::Height,
    ) -> CoinbaseSpendRestriction {
        if self.outputs().is_empty() {
            // we know this transaction must have shielded outputs,
            // because of other consensus rules
            OnlyShieldedOutputs { spend_height }
        } else {
            SomeTransparentOutputs
        }
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
    pub fn lock_time(&self) -> Option<LockTime> {
        let lock_time = match self {
            Transaction::V1 { lock_time, .. }
            | Transaction::V2 { lock_time, .. }
            | Transaction::V3 { lock_time, .. }
            | Transaction::V4 { lock_time, .. }
            | Transaction::V5 { lock_time, .. } => *lock_time,
        };

        // `zcashd` checks that the block height is greater than the lock height.
        // This check allows the genesis block transaction, which would otherwise be invalid.
        // (Or have to use a lock time.)
        //
        // It matches the `zcashd` check here:
        // https://github.com/zcash/zcash/blob/1a7c2a3b04bcad6549be6d571bfdff8af9a2c814/src/main.cpp#L720
        if lock_time == LockTime::unlocked() {
            return None;
        }

        // Consensus rule:
        //
        // > The transaction must be finalized: either its locktime must be in the past (or less
        // > than or equal to the current block height), or all of its sequence numbers must be
        // > 0xffffffff.
        //
        // In `zcashd`, this rule applies to both coinbase and prevout input sequence numbers.
        //
        // Unlike Bitcoin, Zcash allows transactions with no transparent inputs. These transactions
        // only have shielded inputs. Surprisingly, the `zcashd` implementation ignores the lock
        // time in these transactions. `zcashd` only checks the lock time when it finds a
        // transparent input sequence number that is not `u32::MAX`.
        //
        // https://developer.bitcoin.org/devguide/transactions.html#non-standard-transactions
        let has_sequence_number_enabling_lock_time = self
            .inputs()
            .iter()
            .map(transparent::Input::sequence)
            .any(|sequence_number| sequence_number != u32::MAX);

        if has_sequence_number_enabling_lock_time {
            Some(lock_time)
        } else {
            None
        }
    }

    /// Get this transaction's expiry height, if any.
    pub fn expiry_height(&self) -> Option<block::Height> {
        match self {
            Transaction::V1 { .. } | Transaction::V2 { .. } => None,
            Transaction::V3 { expiry_height, .. }
            | Transaction::V4 { expiry_height, .. }
            | Transaction::V5 { expiry_height, .. } => match expiry_height {
                // Consensus rule:
                // > No limit: To set no limit on transactions (so that they do not expire), nExpiryHeight should be set to 0.
                // https://zips.z.cash/zip-0203#specification
                block::Height(0) => None,
                block::Height(expiry_height) => Some(block::Height(*expiry_height)),
            },
        }
    }

    /// Modify the expiry height of this transaction.
    ///
    /// # Panics
    ///
    /// - if called on a v1 or v2 transaction
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn expiry_height_mut(&mut self) -> &mut block::Height {
        match self {
            Transaction::V1 { .. } | Transaction::V2 { .. } => {
                panic!("v1 and v2 transactions are not supported")
            }
            Transaction::V3 {
                ref mut expiry_height,
                ..
            }
            | Transaction::V4 {
                ref mut expiry_height,
                ..
            }
            | Transaction::V5 {
                ref mut expiry_height,
                ..
            } => expiry_height,
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

    /// Access the [`transparent::OutPoint`]s spent by this transaction's [`transparent::Input`]s.
    pub fn spent_outpoints(&self) -> impl Iterator<Item = transparent::OutPoint> + '_ {
        self.inputs()
            .iter()
            .filter_map(transparent::Input::outpoint)
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

    /// Modify the transparent outputs of this transaction, regardless of version.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn outputs_mut(&mut self) -> &mut Vec<transparent::Output> {
        match self {
            Transaction::V1 {
                ref mut outputs, ..
            } => outputs,
            Transaction::V2 {
                ref mut outputs, ..
            } => outputs,
            Transaction::V3 {
                ref mut outputs, ..
            } => outputs,
            Transaction::V4 {
                ref mut outputs, ..
            } => outputs,
            Transaction::V5 {
                ref mut outputs, ..
            } => outputs,
        }
    }

    /// Returns `true` if this transaction has valid inputs for a coinbase
    /// transaction, that is, has a single input and it is a coinbase input.
    pub fn has_valid_coinbase_transaction_inputs(&self) -> bool {
        self.inputs().len() == 1
            && matches!(
                self.inputs().get(0),
                Some(transparent::Input::Coinbase { .. })
            )
    }

    /// Returns `true` if transaction contains any coinbase inputs.
    pub fn has_any_coinbase_inputs(&self) -> bool {
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

    /// Returns the Sprout `JoinSplit<Groth16Proof>`s in this transaction, regardless of version.
    pub fn sprout_groth16_joinsplits(
        &self,
    ) -> Box<dyn Iterator<Item = &sprout::JoinSplit<Groth16Proof>> + '_> {
        match self {
            // JoinSplits with Groth16 Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(joinsplit_data.joinsplits()),

            // No JoinSplits / JoinSplits with BCTV14 proofs
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V5 { .. } => Box::new(std::iter::empty()),
        }
    }

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

    /// Access the JoinSplit public validating key in this transaction,
    /// regardless of version, if any.
    pub fn sprout_joinsplit_pub_key(&self) -> Option<ed25519::VerificationKeyBytes> {
        match self {
            // JoinSplits with Bctv14 Proofs
            Transaction::V2 {
                joinsplit_data: Some(joinsplit_data),
                ..
            }
            | Transaction::V3 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Some(joinsplit_data.pub_key),
            // JoinSplits with Groth Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Some(joinsplit_data.pub_key),
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
            | Transaction::V5 { .. } => None,
        }
    }

    /// Return if the transaction has any Sprout JoinSplit data.
    pub fn has_sprout_joinsplit_data(&self) -> bool {
        match self {
            // No JoinSplits
            Transaction::V1 { .. } | Transaction::V5 { .. } => false,

            // JoinSplits-on-BCTV14
            Transaction::V2 { joinsplit_data, .. } | Transaction::V3 { joinsplit_data, .. } => {
                joinsplit_data.is_some()
            }

            // JoinSplits-on-Groth16
            Transaction::V4 { joinsplit_data, .. } => joinsplit_data.is_some(),
        }
    }

    /// Returns the Sprout note commitments in this transaction.
    pub fn sprout_note_commitments(
        &self,
    ) -> Box<dyn Iterator<Item = &sprout::commitment::NoteCommitment> + '_> {
        match self {
            // Return [`NoteCommitment`]s with [`Bctv14Proof`]s.
            Transaction::V2 {
                joinsplit_data: Some(joinsplit_data),
                ..
            }
            | Transaction::V3 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(joinsplit_data.note_commitments()),

            // Return [`NoteCommitment`]s with [`Groth16Proof`]s.
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(joinsplit_data.note_commitments()),

            // Return an empty iterator.
            Transaction::V2 {
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
            | Transaction::V1 { .. }
            | Transaction::V5 { .. } => Box::new(std::iter::empty()),
        }
    }

    // sapling

    /// Access the deduplicated [`sapling::tree::Root`]s in this transaction,
    /// regardless of version.
    pub fn sapling_anchors(&self) -> Box<dyn Iterator<Item = sapling::tree::Root> + '_> {
        // This function returns a boxed iterator because the different
        // transaction variants end up having different iterator types
        match self {
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.anchors()),

            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.anchors()),

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

    /// Returns the Sapling note commitments in this transaction, regardless of version.
    pub fn sapling_note_commitments(&self) -> Box<dyn Iterator<Item = &jubjub::Fq> + '_> {
        // This function returns a boxed iterator because the different
        // transaction variants end up having different iterator types
        match self {
            // Spends with Groth16 Proofs
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.note_commitments()),
            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Box::new(sapling_shielded_data.note_commitments()),

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

    /// Access the [`orchard::ShieldedData`] in this transaction,
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

    /// Modify the [`orchard::ShieldedData`] in this transaction,
    /// regardless of version.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn orchard_shielded_data_mut(&mut self) -> Option<&mut orchard::ShieldedData> {
        match self {
            Transaction::V5 {
                orchard_shielded_data: Some(orchard_shielded_data),
                ..
            } => Some(orchard_shielded_data),

            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. }
            | Transaction::V5 {
                orchard_shielded_data: None,
                ..
            } => None,
        }
    }

    /// Iterate over the [`orchard::Action`]s in this transaction, if there are any,
    /// regardless of version.
    pub fn orchard_actions(&self) -> impl Iterator<Item = &orchard::Action> {
        self.orchard_shielded_data()
            .into_iter()
            .flat_map(orchard::ShieldedData::actions)
    }

    /// Access the [`orchard::Nullifier`]s in this transaction, if there are any,
    /// regardless of version.
    pub fn orchard_nullifiers(&self) -> impl Iterator<Item = &orchard::Nullifier> {
        self.orchard_shielded_data()
            .into_iter()
            .flat_map(orchard::ShieldedData::nullifiers)
    }

    /// Access the note commitments in this transaction, if there are any,
    /// regardless of version.
    pub fn orchard_note_commitments(&self) -> impl Iterator<Item = &pallas::Base> {
        self.orchard_shielded_data()
            .into_iter()
            .flat_map(orchard::ShieldedData::note_commitments)
    }

    /// Access the [`orchard::Flags`] in this transaction, if there is any,
    /// regardless of version.
    pub fn orchard_flags(&self) -> Option<orchard::shielded_data::Flags> {
        self.orchard_shielded_data()
            .map(|orchard_shielded_data| orchard_shielded_data.flags)
    }

    /// Return if the transaction has any Orchard shielded data,
    /// regardless of version.
    pub fn has_orchard_shielded_data(&self) -> bool {
        self.orchard_shielded_data().is_some()
    }

    // value balances

    /// Return the transparent value balance,
    /// using the outputs spent by this transaction.
    ///
    /// See `transparent_value_balance` for details.
    fn transparent_value_balance_from_outputs(
        &self,
        outputs: &HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        let input_value = self
            .inputs()
            .iter()
            .map(|i| i.value_from_outputs(outputs))
            .sum::<Result<Amount<NonNegative>, AmountError>>()
            .map_err(ValueBalanceError::Transparent)?
            .constrain()
            .expect("conversion from NonNegative to NegativeAllowed is always valid");

        let output_value = self
            .outputs()
            .iter()
            .map(|o| o.value())
            .sum::<Result<Amount<NonNegative>, AmountError>>()
            .map_err(ValueBalanceError::Transparent)?
            .constrain()
            .expect("conversion from NonNegative to NegativeAllowed is always valid");

        (input_value - output_value)
            .map(ValueBalance::from_transparent_amount)
            .map_err(ValueBalanceError::Transparent)
    }

    /// Return the transparent value balance,
    /// the change in the value of the transaction value pool.
    ///
    /// The sum of the UTXOs spent by transparent inputs in `tx_in` fields,
    /// minus the sum of the newly created outputs in `tx_out` fields.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the transparent chain value pool.
    /// Negative values are removed from the transparent chain value pool,
    /// and added to this transaction.
    ///
    /// https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
    ///
    /// `utxos` must contain the utxos of every input in the transaction,
    /// including UTXOs created by earlier transactions in this block.
    #[allow(dead_code)]
    fn transparent_value_balance(
        &self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        self.transparent_value_balance_from_outputs(&outputs_from_utxos(utxos.clone()))
    }

    /// Modify the transparent output values of this transaction, regardless of version.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn output_values_mut(&mut self) -> impl Iterator<Item = &mut Amount<NonNegative>> {
        self.outputs_mut()
            .iter_mut()
            .map(|output| &mut output.value)
    }

    /// Returns the `vpub_old` fields from `JoinSplit`s in this transaction,
    /// regardless of version.
    ///
    /// These values are added to the sprout chain value pool,
    /// and removed from the value pool of this transaction.
    pub fn output_values_to_sprout(&self) -> Box<dyn Iterator<Item = &Amount<NonNegative>> + '_> {
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

    /// Modify the `vpub_old` fields from `JoinSplit`s in this transaction,
    /// regardless of version.
    ///
    /// See `output_values_to_sprout` for details.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn output_values_to_sprout_mut(
        &mut self,
    ) -> Box<dyn Iterator<Item = &mut Amount<NonNegative>> + '_> {
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
                    .joinsplits_mut()
                    .map(|joinsplit| &mut joinsplit.vpub_old),
            ),
            // JoinSplits with Groth16 Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(
                joinsplit_data
                    .joinsplits_mut()
                    .map(|joinsplit| &mut joinsplit.vpub_old),
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

    /// Returns the `vpub_new` fields from `JoinSplit`s in this transaction,
    /// regardless of version.
    ///
    /// These values are removed from the value pool of this transaction.
    /// and added to the sprout chain value pool.
    pub fn input_values_from_sprout(&self) -> Box<dyn Iterator<Item = &Amount<NonNegative>> + '_> {
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
                    .map(|joinsplit| &joinsplit.vpub_new),
            ),
            // JoinSplits with Groth Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(
                joinsplit_data
                    .joinsplits()
                    .map(|joinsplit| &joinsplit.vpub_new),
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

    /// Modify the `vpub_new` fields from `JoinSplit`s in this transaction,
    /// regardless of version.
    ///
    /// See `input_values_from_sprout` for details.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn input_values_from_sprout_mut(
        &mut self,
    ) -> Box<dyn Iterator<Item = &mut Amount<NonNegative>> + '_> {
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
                    .joinsplits_mut()
                    .map(|joinsplit| &mut joinsplit.vpub_new),
            ),
            // JoinSplits with Groth Proofs
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => Box::new(
                joinsplit_data
                    .joinsplits_mut()
                    .map(|joinsplit| &mut joinsplit.vpub_new),
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

    /// Return a list of sprout value balances,
    /// the changes in the transaction value pool due to each sprout [`JoinSplit`].
    ///
    /// Each value balance is the sprout `vpub_new` field, minus the `vpub_old` field.
    ///
    /// See `sprout_value_balance` for details.
    fn sprout_joinsplit_value_balances(
        &self,
    ) -> impl Iterator<Item = ValueBalance<NegativeAllowed>> + '_ {
        let joinsplit_value_balances = match self {
            Transaction::V2 {
                joinsplit_data: Some(joinsplit_data),
                ..
            }
            | Transaction::V3 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.joinsplit_value_balances(),
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.joinsplit_value_balances(),
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
            | Transaction::V5 { .. } => Box::new(iter::empty()),
        };

        joinsplit_value_balances.map(ValueBalance::from_sprout_amount)
    }

    /// Return the sprout value balance,
    /// the change in the transaction value pool due to sprout [`JoinSplit`]s.
    ///
    /// The sum of all sprout `vpub_new` fields, minus the sum of all `vpub_old` fields.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the sprout chain value pool.
    /// Negative values are removed from this transaction,
    /// and added to the sprout pool.
    ///
    /// https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
    fn sprout_value_balance(&self) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        self.sprout_joinsplit_value_balances().sum()
    }

    /// Return the sapling value balance,
    /// the change in the transaction value pool due to sapling `Spend`s and `Output`s.
    ///
    /// Returns the `valueBalanceSapling` field in this transaction.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the sapling chain value pool.
    /// Negative values are removed from this transaction,
    /// and added to sapling pool.
    ///
    /// https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
    pub fn sapling_value_balance(&self) -> ValueBalance<NegativeAllowed> {
        let sapling_value_balance = match self {
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => sapling_shielded_data.value_balance,
            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => sapling_shielded_data.value_balance,

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
            } => Amount::zero(),
        };

        ValueBalance::from_sapling_amount(sapling_value_balance)
    }

    /// Modify the `value_balance` field from the `sapling::ShieldedData` in this transaction,
    /// regardless of version.
    ///
    /// See `sapling_value_balance` for details.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn sapling_value_balance_mut(&mut self) -> Option<&mut Amount<NegativeAllowed>> {
        match self {
            Transaction::V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Some(&mut sapling_shielded_data.value_balance),
            Transaction::V5 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => Some(&mut sapling_shielded_data.value_balance),
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
            } => None,
        }
    }

    /// Return the orchard value balance,
    /// the change in the transaction value pool due to orchard [`Action`]s.
    ///
    /// Returns the `valueBalanceOrchard` field in this transaction.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the orchard chain value pool.
    /// Negative values are removed from this transaction,
    /// and added to orchard pool.
    ///
    /// https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
    pub fn orchard_value_balance(&self) -> ValueBalance<NegativeAllowed> {
        let orchard_value_balance = self
            .orchard_shielded_data()
            .map(|shielded_data| shielded_data.value_balance)
            .unwrap_or_else(Amount::zero);

        ValueBalance::from_orchard_amount(orchard_value_balance)
    }

    /// Modify the `value_balance` field from the `orchard::ShieldedData` in this transaction,
    /// regardless of version.
    ///
    /// See `orchard_value_balance` for details.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn orchard_value_balance_mut(&mut self) -> Option<&mut Amount<NegativeAllowed>> {
        self.orchard_shielded_data_mut()
            .map(|shielded_data| &mut shielded_data.value_balance)
    }

    /// Get the value balances for this transaction,
    /// using the transparent outputs spent in this transaction.
    ///
    /// See `value_balance` for details.
    pub(crate) fn value_balance_from_outputs(
        &self,
        outputs: &HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        self.transparent_value_balance_from_outputs(outputs)?
            + self.sprout_value_balance()?
            + self.sapling_value_balance()
            + self.orchard_value_balance()
    }

    /// Get the value balances for this transaction.
    /// These are the changes in the transaction value pool,
    /// split up into transparent, sprout, sapling, and orchard values.
    ///
    /// Calculated as the sum of the inputs and outputs from each pool,
    /// or the sum of the value balances from each pool.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the corresponding chain value pool.
    /// Negative values are removed from this transaction,
    /// and added to the corresponding pool.
    ///
    /// https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
    ///
    /// `utxos` must contain the utxos of every input in the transaction,
    /// including UTXOs created by earlier transactions in this block.
    ///
    /// Note: the chain value pool has the opposite sign to the transaction
    /// value pool.
    pub fn value_balance(
        &self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        self.value_balance_from_outputs(&outputs_from_utxos(utxos.clone()))
    }
}
