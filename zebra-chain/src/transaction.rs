//! Transactions and transaction-related structures.

use std::{collections::HashMap, fmt, iter, sync::Arc};

use halo2::pasta::pallas;

mod auth_digest;
mod hash;
mod joinsplit;
mod lock_time;
mod memo;
mod serialize;
mod sighash;
mod txid;
mod unmined;

pub mod builder;

#[cfg(any(test, feature = "proptest-impl"))]
#[allow(clippy::unwrap_in_result)]
pub mod arbitrary;
#[cfg(test)]
mod tests;

pub use auth_digest::AuthDigest;
pub use hash::{Hash, WtxId};
pub use joinsplit::JoinSplitData;
pub use lock_time::LockTime;
pub use memo::Memo;
pub use sapling::FieldNotPresent;
pub use serialize::{
    SerializedTransaction, MIN_TRANSPARENT_TX_SIZE, MIN_TRANSPARENT_TX_V4_SIZE,
    MIN_TRANSPARENT_TX_V5_SIZE,
};
pub use sighash::{HashType, SigHash, SigHasher};
pub use unmined::{
    zip317, UnminedTx, UnminedTxId, VerifiedUnminedTx, MEMPOOL_TRANSACTION_COST_THRESHOLD,
};
use zcash_protocol::consensus;

#[cfg(feature = "tx_v6")]
use crate::parameters::TX_V6_VERSION_GROUP_ID;
use crate::{
    amount::{Amount, Error as AmountError, NegativeAllowed, NonNegative},
    block, orchard,
    parameters::{
        Network, NetworkUpgrade, OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID,
        TX_V5_VERSION_GROUP_ID,
    },
    primitives::{ed25519, Bctv14Proof, Groth16Proof},
    sapling,
    serialization::ZcashSerialize,
    sprout,
    transparent::{
        self, outputs_from_utxos,
        CoinbaseSpendRestriction::{self, *},
    },
    value_balance::{ValueBalance, ValueBalanceError},
    Error,
};

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
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    any(test, feature = "proptest-impl", feature = "elasticsearch"),
    derive(Serialize)
)]
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
    /// A `version = 6` transaction, which is reserved for current development.
    #[cfg(feature = "tx_v6")]
    V6 {
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
        // TODO: Add the rest of the v6 fields.
    },
}

impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct("Transaction");

        fmter.field("version", &self.version());

        if let Some(network_upgrade) = self.network_upgrade() {
            fmter.field("network_upgrade", &network_upgrade);
        }

        if let Some(lock_time) = self.lock_time() {
            fmter.field("lock_time", &lock_time);
        }

        if let Some(expiry_height) = self.expiry_height() {
            fmter.field("expiry_height", &expiry_height);
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

    /// Calculate the sighash for the current transaction.
    ///
    /// If you need to compute multiple sighashes for the same transactions,
    /// it's more efficient to use [`Transaction::sighasher()`].
    ///
    /// # Details
    ///
    /// `all_previous_outputs` represents the UTXOs being spent by each input
    /// in the transaction.
    ///
    /// The `input_index_script_code` tuple indicates the index of the
    /// transparent Input for which we are producing a sighash and the
    /// respective script code being validated, or None if it's a shielded
    /// input.
    ///
    /// # Panics
    ///
    /// - if passed in any NetworkUpgrade from before NetworkUpgrade::Overwinter
    /// - if called on a v1 or v2 transaction
    /// - if the input index points to a transparent::Input::CoinBase
    /// - if the input index is out of bounds for self.inputs()
    /// - if the tx contains `nConsensusBranchId` field and `nu` doesn't match it
    /// - if the tx is not convertible to its `librustzcash` equivalent
    /// - if `nu` doesn't contain a consensus branch id convertible to its `librustzcash`
    ///   equivalent
    pub fn sighash(
        &self,
        nu: NetworkUpgrade,
        hash_type: sighash::HashType,
        all_previous_outputs: Arc<Vec<transparent::Output>>,
        input_index_script_code: Option<(usize, Vec<u8>)>,
    ) -> Result<SigHash, Error> {
        Ok(sighash::SigHasher::new(self, nu, all_previous_outputs)?
            .sighash(hash_type, input_index_script_code))
    }

    /// Return a [`SigHasher`] for this transaction.
    pub fn sighasher(
        &self,
        nu: NetworkUpgrade,
        all_previous_outputs: Arc<Vec<transparent::Output>>,
    ) -> Result<sighash::SigHasher, Error> {
        sighash::SigHasher::new(self, nu, all_previous_outputs)
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Some(AuthDigest::from(self)),
        }
    }

    // other properties

    /// Does this transaction have transparent inputs?
    pub fn has_transparent_inputs(&self) -> bool {
        !self.inputs().is_empty()
    }

    /// Does this transaction have transparent outputs?
    pub fn has_transparent_outputs(&self) -> bool {
        !self.outputs().is_empty()
    }

    /// Does this transaction have transparent inputs or outputs?
    pub fn has_transparent_inputs_or_outputs(&self) -> bool {
        self.has_transparent_inputs() || self.has_transparent_outputs()
    }

    /// Does this transaction have transparent or shielded inputs?
    pub fn has_transparent_or_shielded_inputs(&self) -> bool {
        self.has_transparent_inputs() || self.has_shielded_inputs()
    }

    /// Does this transaction have shielded inputs?
    ///
    /// See [`Self::has_transparent_or_shielded_inputs`] for details.
    pub fn has_shielded_inputs(&self) -> bool {
        self.joinsplit_count() > 0
            || self.sapling_spends_per_anchor().count() > 0
            || (self.orchard_actions().count() > 0
                && self
                    .orchard_flags()
                    .unwrap_or_else(orchard::Flags::empty)
                    .contains(orchard::Flags::ENABLE_SPENDS))
    }

    /// Does this transaction have shielded outputs?
    ///
    /// See [`Self::has_transparent_or_shielded_outputs`] for details.
    pub fn has_shielded_outputs(&self) -> bool {
        self.joinsplit_count() > 0
            || self.sapling_outputs().count() > 0
            || (self.orchard_actions().count() > 0
                && self
                    .orchard_flags()
                    .unwrap_or_else(orchard::Flags::empty)
                    .contains(orchard::Flags::ENABLE_OUTPUTS))
    }

    /// Does this transaction have transparent or shielded outputs?
    pub fn has_transparent_or_shielded_outputs(&self) -> bool {
        self.has_transparent_outputs() || self.has_shielded_outputs()
    }

    /// Does this transaction has at least one flag when we have at least one orchard action?
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
        network: &Network,
        spend_height: block::Height,
    ) -> CoinbaseSpendRestriction {
        if self.outputs().is_empty() || network.should_allow_unshielded_coinbase_spends() {
            // we know this transaction must have shielded outputs if it has no
            // transparent outputs, because of other consensus rules.
            CheckCoinbaseMaturity { spend_height }
        } else {
            DisallowCoinbaseSpend
        }
    }

    // header

    /// Return if the `fOverwintered` flag of this transaction is set.
    pub fn is_overwintered(&self) -> bool {
        match self {
            Transaction::V1 { .. } | Transaction::V2 { .. } => false,
            Transaction::V3 { .. } | Transaction::V4 { .. } | Transaction::V5 { .. } => true,
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => true,
        }
    }

    /// Returns the version of this transaction.
    ///
    /// Note that the returned version is equal to `effectiveVersion`, described in [§ 7.1
    /// Transaction Encoding and Consensus]:
    ///
    /// > `effectiveVersion` [...] is equal to `min(2, version)` when `fOverwintered = 0` and to
    /// > `version` otherwise.
    ///
    /// Zebra handles the `fOverwintered` flag via the [`Self::is_overwintered`] method.
    ///
    /// [§ 7.1 Transaction Encoding and Consensus]: <https://zips.z.cash/protocol/protocol.pdf#txnencoding>
    pub fn version(&self) -> u32 {
        match self {
            Transaction::V1 { .. } => 1,
            Transaction::V2 { .. } => 2,
            Transaction::V3 { .. } => 3,
            Transaction::V4 { .. } => 4,
            Transaction::V5 { .. } => 5,
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => 6,
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { lock_time, .. } => *lock_time,
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

    /// Get the raw lock time value.
    pub fn raw_lock_time(&self) -> u32 {
        let lock_time = match self {
            Transaction::V1 { lock_time, .. }
            | Transaction::V2 { lock_time, .. }
            | Transaction::V3 { lock_time, .. }
            | Transaction::V4 { lock_time, .. }
            | Transaction::V5 { lock_time, .. } => *lock_time,
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { lock_time, .. } => *lock_time,
        };
        let mut lock_time_bytes = Vec::new();
        lock_time
            .zcash_serialize(&mut lock_time_bytes)
            .expect("lock_time should serialize");
        u32::from_le_bytes(
            lock_time_bytes
                .try_into()
                .expect("should serialize as 4 bytes"),
        )
    }

    /// Returns `true` if this transaction's `lock_time` is a [`LockTime::Time`].
    /// Returns `false` if it is a [`LockTime::Height`] (locked or unlocked), is unlocked,
    /// or if the transparent input sequence numbers have disabled lock times.
    pub fn lock_time_is_time(&self) -> bool {
        if let Some(lock_time) = self.lock_time() {
            return lock_time.is_time();
        }

        false
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { expiry_height, .. } => match expiry_height {
                // # Consensus
                //
                // > No limit: To set no limit on transactions (so that they do not expire), nExpiryHeight should be set to 0.
                // https://zips.z.cash/zip-0203#specification
                block::Height(0) => None,
                block::Height(expiry_height) => Some(block::Height(*expiry_height)),
            },
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { ref inputs, .. } => inputs,
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { ref outputs, .. } => outputs,
        }
    }

    /// Returns `true` if this transaction has valid inputs for a coinbase
    /// transaction, that is, has a single input and it is a coinbase input
    /// (null prevout).
    pub fn is_coinbase(&self) -> bool {
        self.inputs().len() == 1
            && matches!(
                self.inputs().first(),
                Some(transparent::Input::Coinbase { .. })
            )
    }

    /// Returns `true` if this transaction has valid inputs for a non-coinbase
    /// transaction, that is, does not have any coinbase input (non-null prevouts).
    ///
    /// Note that it's possible for a transaction return false in both
    /// [`Transaction::is_coinbase`] and [`Transaction::is_valid_non_coinbase`],
    /// though those transactions will be rejected.
    pub fn is_valid_non_coinbase(&self) -> bool {
        self.inputs()
            .iter()
            .all(|input| matches!(input, transparent::Input::PrevOut { .. }))
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => 0,
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => None,
        }
    }

    /// Return if the transaction has any Sprout JoinSplit data.
    pub fn has_sprout_joinsplit_data(&self) -> bool {
        match self {
            // No JoinSplits
            Transaction::V1 { .. } | Transaction::V5 { .. } => false,
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => false,

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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
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

            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
    #[allow(clippy::unwrap_in_result)]
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

    /// Returns the `vpub_old` fields from `JoinSplit`s in this transaction,
    /// regardless of version, in the order they appear in the transaction.
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Returns the `vpub_new` fields from `JoinSplit`s in this transaction,
    /// regardless of version, in the order they appear in the transaction.
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Return a list of sprout value balances,
    /// the changes in the transaction value pool due to each sprout `JoinSplit`.
    ///
    /// Each value balance is the sprout `vpub_new` field, minus the `vpub_old` field.
    ///
    /// See [`sprout_value_balance`][svb] for details.
    ///
    /// [svb]: crate::transaction::Transaction::sprout_value_balance
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(iter::empty()),
        };

        joinsplit_value_balances.map(ValueBalance::from_sprout_amount)
    }

    /// Return the sprout value balance,
    /// the change in the transaction value pool due to sprout `JoinSplit`s.
    ///
    /// The sum of all sprout `vpub_new` fields, minus the sum of all `vpub_old` fields.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the sprout chain value pool.
    /// Negative values are removed from this transaction,
    /// and added to the sprout pool.
    ///
    /// <https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions>
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
    /// <https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions>
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                sapling_shielded_data: None,
                ..
            } => Amount::zero(),
        };

        ValueBalance::from_sapling_amount(sapling_value_balance)
    }

    /// Return the orchard value balance, the change in the transaction value
    /// pool due to [`orchard::Action`]s.
    ///
    /// Returns the `valueBalanceOrchard` field in this transaction.
    ///
    /// Positive values are added to this transaction's value pool,
    /// and removed from the orchard chain value pool.
    /// Negative values are removed from this transaction,
    /// and added to orchard pool.
    ///
    /// <https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions>
    pub fn orchard_value_balance(&self) -> ValueBalance<NegativeAllowed> {
        let orchard_value_balance = self
            .orchard_shielded_data()
            .map(|shielded_data| shielded_data.value_balance)
            .unwrap_or_else(Amount::zero);

        ValueBalance::from_orchard_amount(orchard_value_balance)
    }

    /// Returns the value balances for this transaction using the provided transparent outputs.
    pub(crate) fn value_balance_from_outputs(
        &self,
        outputs: &HashMap<transparent::OutPoint, transparent::Output>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        self.transparent_value_balance_from_outputs(outputs)?
            + self.sprout_value_balance()?
            + self.sapling_value_balance()
            + self.orchard_value_balance()
    }

    /// Returns the value balances for this transaction.
    ///
    /// These are the changes in the transaction value pool, split up into transparent, Sprout,
    /// Sapling, and Orchard values.
    ///
    /// Calculated as the sum of the inputs and outputs from each pool, or the sum of the value
    /// balances from each pool.
    ///
    /// Positive values are added to this transaction's value pool, and removed from the
    /// corresponding chain value pool. Negative values are removed from this transaction, and added
    /// to the corresponding pool.
    ///
    /// <https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions>
    ///
    /// `utxos` must contain the utxos of every input in the transaction, including UTXOs created by
    /// earlier transactions in this block.
    ///
    /// ## Note
    ///
    /// The chain value pool has the opposite sign to the transaction value pool.
    pub fn value_balance(
        &self,
        utxos: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<ValueBalance<NegativeAllowed>, ValueBalanceError> {
        self.value_balance_from_outputs(&outputs_from_utxos(utxos.clone()))
    }

    /// Converts [`Transaction`] to [`zcash_primitives::transaction::Transaction`].
    ///
    /// If the tx contains a network upgrade, this network upgrade must match the passed `nu`. The
    /// passed `nu` must also contain a consensus branch id convertible to its `librustzcash`
    /// equivalent.
    pub(crate) fn to_librustzcash(
        &self,
        nu: NetworkUpgrade,
    ) -> Result<zcash_primitives::transaction::Transaction, crate::Error> {
        if self.network_upgrade().is_some_and(|tx_nu| tx_nu != nu) {
            return Err(crate::Error::InvalidConsensusBranchId);
        }

        let Some(branch_id) = nu.branch_id() else {
            return Err(crate::Error::InvalidConsensusBranchId);
        };

        let Ok(branch_id) = consensus::BranchId::try_from(branch_id) else {
            return Err(crate::Error::InvalidConsensusBranchId);
        };

        Ok(zcash_primitives::transaction::Transaction::read(
            &self.zcash_serialize_to_vec()?[..],
            branch_id,
        )?)
    }

    // Common Sapling & Orchard Properties

    /// Does this transaction have shielded inputs or outputs?
    pub fn has_shielded_data(&self) -> bool {
        self.has_shielded_inputs() || self.has_shielded_outputs()
    }

    /// Get the version group ID for this transaction, if any.
    pub fn version_group_id(&self) -> Option<u32> {
        // We could store the parsed version group ID and return that,
        // but since the consensus rules constraint it, we can just return
        // the value that must have been parsed.
        match self {
            Transaction::V1 { .. } | Transaction::V2 { .. } => None,
            Transaction::V3 { .. } => Some(OVERWINTER_VERSION_GROUP_ID),
            Transaction::V4 { .. } => Some(SAPLING_VERSION_GROUP_ID),
            Transaction::V5 { .. } => Some(TX_V5_VERSION_GROUP_ID),
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Some(TX_V6_VERSION_GROUP_ID),
        }
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Transaction {
    /// Updates the [`NetworkUpgrade`] for this transaction.
    ///
    /// ## Notes
    ///
    /// - Updating the network upgrade for V1, V2, V3 and V4 transactions is not possible.
    pub fn update_network_upgrade(&mut self, nu: NetworkUpgrade) -> Result<(), &str> {
        match self {
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => Err(
                "Updating the network upgrade for V1, V2, V3 and V4 transactions is not possible.",
            ),
            Transaction::V5 {
                ref mut network_upgrade,
                ..
            } => {
                *network_upgrade = nu;
                Ok(())
            }
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                ref mut network_upgrade,
                ..
            } => {
                *network_upgrade = nu;
                Ok(())
            }
        }
    }

    /// Modify the expiry height of this transaction.
    ///
    /// # Panics
    ///
    /// - if called on a v1 or v2 transaction
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                ref mut expiry_height,
                ..
            } => expiry_height,
        }
    }

    /// Modify the transparent inputs of this transaction, regardless of version.
    pub fn inputs_mut(&mut self) -> &mut Vec<transparent::Input> {
        match self {
            Transaction::V1 { ref mut inputs, .. } => inputs,
            Transaction::V2 { ref mut inputs, .. } => inputs,
            Transaction::V3 { ref mut inputs, .. } => inputs,
            Transaction::V4 { ref mut inputs, .. } => inputs,
            Transaction::V5 { ref mut inputs, .. } => inputs,
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { ref mut inputs, .. } => inputs,
        }
    }

    /// Modify the `value_balance` field from the `orchard::ShieldedData` in this transaction,
    /// regardless of version.
    ///
    /// See `orchard_value_balance` for details.
    pub fn orchard_value_balance_mut(&mut self) -> Option<&mut Amount<NegativeAllowed>> {
        self.orchard_shielded_data_mut()
            .map(|shielded_data| &mut shielded_data.value_balance)
    }

    /// Modify the `value_balance` field from the `sapling::ShieldedData` in this transaction,
    /// regardless of version.
    ///
    /// See `sapling_value_balance` for details.
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                sapling_shielded_data: None,
                ..
            } => None,
        }
    }

    /// Modify the `vpub_new` fields from `JoinSplit`s in this transaction,
    /// regardless of version, in the order they appear in the transaction.
    ///
    /// See `input_values_from_sprout` for details.
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Modify the `vpub_old` fields from `JoinSplit`s in this transaction,
    /// regardless of version, in the order they appear in the transaction.
    ///
    /// See `output_values_to_sprout` for details.
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => Box::new(std::iter::empty()),
        }
    }

    /// Modify the transparent output values of this transaction, regardless of version.
    pub fn output_values_mut(&mut self) -> impl Iterator<Item = &mut Amount<NonNegative>> {
        self.outputs_mut()
            .iter_mut()
            .map(|output| &mut output.value)
    }

    /// Modify the [`orchard::ShieldedData`] in this transaction,
    /// regardless of version.
    pub fn orchard_shielded_data_mut(&mut self) -> Option<&mut orchard::ShieldedData> {
        match self {
            Transaction::V5 {
                orchard_shielded_data: Some(orchard_shielded_data),
                ..
            } => Some(orchard_shielded_data),
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                orchard_shielded_data: None,
                ..
            } => None,
        }
    }

    /// Modify the transparent outputs of this transaction, regardless of version.
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
            #[cfg(feature = "tx_v6")]
            Transaction::V6 {
                ref mut outputs, ..
            } => outputs,
        }
    }
}
