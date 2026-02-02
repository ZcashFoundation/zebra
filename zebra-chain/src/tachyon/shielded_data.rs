//! Tachyon shielded data for transactions.
//!
//! This module defines two types for Tachyon transactions:
//!
//! - [`ShieldedData`] - Regular tachyon transaction data that references an aggregate
//! - [`AggregateData`] - Aggregate transaction data containing the proof
//!
//! ## Aggregate Transaction Model
//!
//! Tachyon uses an aggregate proof model where:
//! - **Aggregate transactions** contain an [`AggregateProof`] covering multiple tachyon txs
//! - **Regular tachyon transactions** contain [`ShieldedData`] and reference an aggregate
//!   by transaction hash
//!
//! Multiple aggregate transactions may exist per block, and regular tachyon
//! transactions specify which aggregate covers them via `aggregate_ref`.

use std::fmt;

use bitflags::bitflags;
use group::ff::PrimeField;
use halo2::pasta::pallas;
use reddsa::{orchard::Binding, Signature};

use crate::{
    amount::{Amount, NegativeAllowed},
    serialization::{AtLeastOne, TrustedPreallocate},
    transaction,
};

use super::{
    accumulator,
    action::{AuthorizedTachyaction, Tachyaction},
    commitment::ValueCommitment,
    nullifier::FlavoredNullifier,
    proof::AggregateProof,
    tachygram::Tachygram,
};

bitflags! {
    /// Flags for Tachyon transactions.
    ///
    /// Similar to Orchard flags, these control which operations are enabled.
    ///
    /// In a Tachyon transaction, the reserved bits 2..7 of the flagsTachyon
    /// field MUST be zero.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct Flags: u8 {
        /// Enable spending non-zero valued notes (requires at least one spend).
        const ENABLE_SPENDS = 0b0000_0001;
        /// Enable creating new non-zero valued notes (requires at least one output).
        const ENABLE_OUTPUTS = 0b0000_0010;
        // Bits 2..7 are reserved and must be zero.
    }
}

// Manual serde implementation using bitflags_serde_legacy for compatibility
impl serde::Serialize for Flags {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        bitflags_serde_legacy::serialize(self, "Flags", serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Flags {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        bitflags_serde_legacy::deserialize("Flags", deserializer)
    }
}

impl Flags {
    /// Check if spends are enabled.
    pub fn spends_enabled(&self) -> bool {
        self.contains(Self::ENABLE_SPENDS)
    }

    /// Check if outputs are enabled.
    pub fn outputs_enabled(&self) -> bool {
        self.contains(Self::ENABLE_OUTPUTS)
    }
}

impl Default for Flags {
    fn default() -> Self {
        Self::ENABLE_SPENDS | Self::ENABLE_OUTPUTS
    }
}

/// Tachyon shielded data bundle for a regular tachyon transaction.
///
/// This is the main container for Tachyon-related data in a transaction that
/// performs spends/outputs. It references an aggregate transaction that
/// contains the proof covering this transaction's validity.
///
/// Unlike Orchard's `ShieldedData` which contains its own proof, tachyon
/// transactions reference an [`AggregateData`] transaction via `aggregate_ref`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShieldedData {
    /// Transaction flags (enable spends/outputs).
    pub flags: Flags,

    /// Net value of Tachyon spends minus outputs.
    ///
    /// Positive means value flows out of Tachyon pool (deshielding).
    /// Negative means value flows into Tachyon pool (shielding).
    pub value_balance: Amount<NegativeAllowed>,

    /// Shared anchor for all spends (Tachyon accumulator state).
    ///
    /// All spends in this transaction must reference notes committed
    /// at or before this accumulator state.
    pub shared_anchor: accumulator::Anchor,

    /// Reference to the aggregate transaction that proves this bundle.
    ///
    /// The aggregate transaction must be in the same block and its
    /// [`AggregateData::covered_transactions`] must include this transaction.
    pub aggregate_ref: transaction::Hash,

    /// The Tachyactions with authorization signatures.
    ///
    /// Each action represents a spend and/or output. Actions may be
    /// dummy actions (spending zero value from dummy notes) to hide
    /// the true number of spends/outputs.
    pub actions: AtLeastOne<AuthorizedTachyaction>,

    /// Binding signature on transaction sighash.
    ///
    /// This proves that the value commitments in actions sum to the
    /// declared value_balance without revealing actual amounts.
    pub binding_sig: Signature<Binding>,
}

/// Aggregate transaction data containing the proof for multiple tachyon transactions.
///
/// This type is used in "aggregate transactions" that exist solely to carry
/// the aggregated Ragu proof covering a set of regular tachyon transactions.
///
/// Multiple aggregate transactions may exist per block, each covering a
/// different subset of tachyon transactions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateData {
    /// The aggregated Ragu proof covering all referenced transactions.
    pub proof: AggregateProof,

    /// List of transaction hashes covered by this aggregate proof.
    ///
    /// Each transaction in this list must:
    /// - Be in the same block as this aggregate transaction
    /// - Have its `ShieldedData::aggregate_ref` point to this aggregate
    pub covered_transactions: Vec<transaction::Hash>,
}

impl AggregateData {
    /// Create a new aggregate data with the given proof and covered transactions.
    pub fn new(proof: AggregateProof, covered_transactions: Vec<transaction::Hash>) -> Self {
        Self {
            proof,
            covered_transactions,
        }
    }

    /// Create an empty aggregate (for testing purposes).
    pub fn empty() -> Self {
        Self {
            proof: AggregateProof::empty(),
            covered_transactions: Vec::new(),
        }
    }

    /// Get the number of transactions covered by this aggregate.
    pub fn covered_count(&self) -> usize {
        self.covered_transactions.len()
    }

    /// Check if this aggregate covers a specific transaction.
    pub fn covers(&self, tx_hash: &transaction::Hash) -> bool {
        self.covered_transactions.contains(tx_hash)
    }
}

impl fmt::Display for ShieldedData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("tachyon::ShieldedData")
            .field("actions", &self.actions.len())
            .field("value_balance", &self.value_balance)
            .field("flags", &self.flags)
            .field("aggregate_ref", &self.aggregate_ref)
            .finish()
    }
}

impl fmt::Display for AggregateData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("tachyon::AggregateData")
            .field("proof_size", &self.proof.size())
            .field("covered_transactions", &self.covered_transactions.len())
            .finish()
    }
}

impl ShieldedData {
    /// Iterate over the actions in this bundle.
    pub fn actions(&self) -> impl Iterator<Item = &Tachyaction> {
        self.actions.iter().map(|aa| &aa.action)
    }

    /// Collect the nullifiers from this bundle.
    pub fn nullifiers(&self) -> impl Iterator<Item = &FlavoredNullifier> {
        self.actions().map(|action| &action.nullifier)
    }

    /// Collect the note commitment x-coordinates from this bundle.
    pub fn note_commitments(&self) -> impl Iterator<Item = &pallas::Base> {
        self.actions().map(|action| &action.cm_x)
    }

    /// Generate tachygrams for all nullifiers and commitments in this bundle.
    ///
    /// Returns an iterator of tachygrams in order: all nullifiers first,
    /// then all note commitments. This ordering is important for the
    /// Tachyon accumulator.
    pub fn tachygrams(&self) -> impl Iterator<Item = Tachygram> + '_ {
        // Nullifiers as tachygrams (extract the nullifier from FlavoredNullifier)
        let nullifier_tachygrams = self
            .nullifiers()
            .map(|fnf| Tachygram::from_nullifier(&fnf.nullifier()));

        // Note commitments as tachygrams
        let commitment_tachygrams = self.note_commitments().map(|cm_x| {
            let bytes = cm_x.to_repr();
            Tachygram::from_bytes(bytes)
        });

        nullifier_tachygrams.chain(commitment_tachygrams)
    }

    /// Get the value balance of this bundle.
    pub fn value_balance(&self) -> Amount<NegativeAllowed> {
        self.value_balance
    }

    /// Calculate the binding verification key.
    ///
    /// This is used to verify the binding signature. The key is derived from
    /// the value commitments in actions and the balancing value.
    pub fn binding_verification_key(&self) -> reddsa::VerificationKeyBytes<Binding> {
        let cv: ValueCommitment = self.actions().map(|action| action.cv).sum();
        let cv_balance = ValueCommitment::new(pallas::Scalar::zero(), self.value_balance);

        let key_bytes: [u8; 32] = (cv - cv_balance).into();
        key_bytes.into()
    }

    /// Count the number of actions in this bundle.
    pub fn actions_count(&self) -> usize {
        self.actions.len()
    }
}

impl AtLeastOne<AuthorizedTachyaction> {
    /// Iterate over the actions.
    pub fn actions(&self) -> impl Iterator<Item = &Tachyaction> {
        self.iter().map(|aa| &aa.action)
    }
}

// TrustedPreallocate implementation for bounded deserialization
impl TrustedPreallocate for AuthorizedTachyaction {
    fn max_allocation() -> u64 {
        // Same logic as Orchard: limit based on max block size
        // Each action is ~196 bytes, max block is ~2MB
        const MAX: u64 = (2_000_000 / AuthorizedTachyaction::SIZE as u64) + 1;
        MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_default() {
        let _init_guard = zebra_test::init();

        let flags = Flags::default();
        assert!(flags.spends_enabled());
        assert!(flags.outputs_enabled());
    }

    #[test]
    fn flags_individual() {
        let _init_guard = zebra_test::init();

        let spend_only = Flags::ENABLE_SPENDS;
        assert!(spend_only.spends_enabled());
        assert!(!spend_only.outputs_enabled());

        let output_only = Flags::ENABLE_OUTPUTS;
        assert!(!output_only.spends_enabled());
        assert!(output_only.outputs_enabled());
    }

    #[test]
    fn aggregate_data_covers() {
        let _init_guard = zebra_test::init();

        let tx1 = transaction::Hash([1u8; 32]);
        let tx2 = transaction::Hash([2u8; 32]);
        let tx3 = transaction::Hash([3u8; 32]);

        let aggregate = AggregateData::new(AggregateProof::empty(), vec![tx1, tx2]);

        assert!(aggregate.covers(&tx1));
        assert!(aggregate.covers(&tx2));
        assert!(!aggregate.covers(&tx3));
        assert_eq!(aggregate.covered_count(), 2);
    }
}
