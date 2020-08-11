//! Transaction types.

use serde::{Deserialize, Serialize};

mod hash;
mod joinsplit;
mod serialize;
mod shielded_data;
mod transparent;

#[cfg(test)]
mod tests;

pub use hash::TransactionHash;
pub use joinsplit::{JoinSplit, JoinSplitData};
pub use shielded_data::{Output, ShieldedData, Spend};
pub use transparent::{CoinbaseData, OutPoint, TransparentInput, TransparentOutput};

use crate::proofs::{Bctv14Proof, Groth16Proof};
use crate::{
    parameters::ConsensusBranchId,
    serialization::ZcashSerialize,
    types::{amount::Amount, BlockHeight, LockTime},
    Network, NetworkUpgrade,
};
use blake2b_simd::Hash;
use byteorder::{LittleEndian, WriteBytesExt};

const OVERWINTER_VERSION_GROUP_ID: u32 = 0x03C4_8270;
const SAPLING_VERSION_GROUP_ID: u32 = 0x892F_2085;

/// A Zcash transaction.
///
/// A transaction is an encoded data structure that facilitates the transfer of
/// value between two public key addresses on the Zcash ecosystem. Everything is
/// designed to ensure that transactions can created, propagated on the network,
/// validated, and finally added to the global ledger of transactions (the
/// blockchain).
///
/// Zcash has a number of different transaction formats. They are represented
/// internally by different enum variants. Because we checkpoint on Sapling
/// activation, we do not parse any pre-Sapling transaction types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// XXX consider boxing the Optional fields of V4 txs
#[allow(clippy::large_enum_variant)]
pub enum Transaction {
    /// A fully transparent transaction (`version = 1`).
    V1 {
        /// The transparent inputs to the transaction.
        inputs: Vec<TransparentInput>,
        /// The transparent outputs from the transaction.
        outputs: Vec<TransparentOutput>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
    },
    /// A Sprout transaction (`version = 2`).
    V2 {
        /// The transparent inputs to the transaction.
        inputs: Vec<TransparentInput>,
        /// The transparent outputs from the transaction.
        outputs: Vec<TransparentOutput>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Bctv14Proof>>,
    },
    /// An Overwinter transaction (`version = 3`).
    V3 {
        /// The transparent inputs to the transaction.
        inputs: Vec<TransparentInput>,
        /// The transparent outputs from the transaction.
        outputs: Vec<TransparentOutput>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The latest block height that this transaction can be added to the chain.
        expiry_height: BlockHeight,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Bctv14Proof>>,
    },
    /// A Sapling transaction (`version = 4`).
    V4 {
        /// The transparent inputs to the transaction.
        inputs: Vec<TransparentInput>,
        /// The transparent outputs from the transaction.
        outputs: Vec<TransparentOutput>,
        /// The earliest time or block height that this transaction can be added to the
        /// chain.
        lock_time: LockTime,
        /// The latest block height that this transaction can be added to the chain.
        expiry_height: BlockHeight,
        /// The net value of Sapling spend transfers minus output transfers.
        value_balance: Amount,
        /// The shielded data for this transaction, if any.
        shielded_data: Option<ShieldedData>,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
    },
}

impl Transaction {
    /// Iterate over the transparent inputs of this transaction, if any.
    pub fn inputs(&self) -> impl Iterator<Item = &TransparentInput> {
        match self {
            Transaction::V1 { ref inputs, .. } => inputs.iter(),
            Transaction::V2 { ref inputs, .. } => inputs.iter(),
            Transaction::V3 { ref inputs, .. } => inputs.iter(),
            Transaction::V4 { ref inputs, .. } => inputs.iter(),
        }
    }

    /// Iterate over the transparent outputs of this transaction, if any.
    pub fn outputs(&self) -> impl Iterator<Item = &TransparentOutput> {
        match self {
            Transaction::V1 { ref outputs, .. } => outputs.iter(),
            Transaction::V2 { ref outputs, .. } => outputs.iter(),
            Transaction::V3 { ref outputs, .. } => outputs.iter(),
            Transaction::V4 { ref outputs, .. } => outputs.iter(),
        }
    }

    /// Get this transaction's lock time.
    pub fn lock_time(&self) -> LockTime {
        match self {
            Transaction::V1 { lock_time, .. } => *lock_time,
            Transaction::V2 { lock_time, .. } => *lock_time,
            Transaction::V3 { lock_time, .. } => *lock_time,
            Transaction::V4 { lock_time, .. } => *lock_time,
        }
    }

    /// Get this transaction's expiry height, if any.
    pub fn expiry_height(&self) -> Option<BlockHeight> {
        match self {
            Transaction::V1 { .. } => None,
            Transaction::V2 { .. } => None,
            Transaction::V3 { expiry_height, .. } => Some(*expiry_height),
            Transaction::V4 { expiry_height, .. } => Some(*expiry_height),
        }
    }

    /// Returns `true` if transaction contains any coinbase inputs.
    pub fn contains_coinbase_input(&self) -> bool {
        self.inputs()
            .any(|input| matches!(input, TransparentInput::Coinbase { .. }))
    }

    // TODO(jlusby): refine type
    #[allow(unused_variables)]
    pub fn sighash(&self, network: Network, height: BlockHeight, hash_type: u32) -> Hash {
        SigHasher {
            trans: self,
            network,
            height,
            hash_type,
        }
        .sighash()
    }

    fn header(&self) -> u32 {
        match self {
            Transaction::V1 { .. } => 1,
            Transaction::V2 { .. } => 2,
            Transaction::V3 { .. } => 3 | 1 << 31,
            Transaction::V4 { .. } => 4 | 1 << 31,
        }
    }

    fn group_id(&self) -> Option<u32> {
        match self {
            Transaction::V1 { .. } => None,
            Transaction::V2 { .. } => None,
            Transaction::V3 { .. } => Some(OVERWINTER_VERSION_GROUP_ID),
            Transaction::V4 { .. } => Some(SAPLING_VERSION_GROUP_ID),
        }
    }
}

const ZCASH_SIGHASH_PERSONALIZATION_PREFIX: &[u8; 12] = b"ZcashSigHash";
const ZCASH_PREVOUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashPrevoutHash";
const ZCASH_SEQUENCE_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSequencHash";
const ZCASH_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashOutputsHash";
const ZCASH_JOINSPLITS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashJSplitsHash";
const ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSSpendsHash";
const ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSOutputHash";

pub const SIGHASH_ALL: u32 = 1;
const SIGHASH_NONE: u32 = 2;
const SIGHASH_SINGLE: u32 = 3;
const SIGHASH_MASK: u32 = 0x1f;
const SIGHASH_ANYONECANPAY: u32 = 0x80;

struct SigHasher<'a> {
    trans: &'a Transaction,
    hash_type: u32,
    network: Network,
    height: BlockHeight,
}

impl<'a> SigHasher<'a> {
    fn sighash(self) -> Hash {
        match self.network_upgrade() {
            NetworkUpgrade::Genesis => unimplemented!(),
            NetworkUpgrade::BeforeOverwinter => unimplemented!(),
            NetworkUpgrade::Overwinter | NetworkUpgrade::Sapling => self.sighash_zip143(),
            NetworkUpgrade::Blossom => unimplemented!(),
            NetworkUpgrade::Heartwood => unimplemented!(),
            NetworkUpgrade::Canopy => unimplemented!(),
        }
    }

    fn network_upgrade(&self) -> NetworkUpgrade {
        NetworkUpgrade::current(self.network, self.height)
    }

    fn consensus_branch_id(&self) -> ConsensusBranchId {
        self.network_upgrade().branch_id().unwrap()
    }

    /// Sighash implementation for the overwinter and sapling consensus branches
    fn sighash_zip143(&self) -> Hash {
        let mut personal = [0; 16];
        (&mut personal[..12]).copy_from_slice(ZCASH_SIGHASH_PERSONALIZATION_PREFIX);
        (&mut personal[12..])
            .write_u32::<LittleEndian>(self.consensus_branch_id().into())
            .unwrap();

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(&personal)
            .to_state();

        hash.update(&self.trans.header().to_le_bytes());
        hash.update(
            &self
                .trans
                .group_id()
                .expect("fOverwintered is always set")
                .to_le_bytes(),
        );
        hash.update(
            self.hash_prevouts()
                .as_ref()
                .map(|h| h.as_ref())
                .unwrap_or(&[0; 32]),
        );
        hash.update(
            self.sequence_hash()
                .as_ref()
                .map(|h| h.as_ref())
                .unwrap_or(&[0; 32]),
        );

        let hash = hash.finalize();

        hash
    }

    fn hash_prevouts(&self) -> Option<Hash> {
        if self.hash_type & SIGHASH_ANYONECANPAY == 0 {
            return None;
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_PREVOUTS_HASH_PERSONALIZATION)
            .to_state();

        let mut buf = vec![];

        self.trans
            .inputs()
            .filter_map(|input| match input {
                TransparentInput::PrevOut { outpoint, .. } => Some(outpoint),
                TransparentInput::Coinbase { .. } => None,
            })
            .try_for_each(|outpoint| outpoint.zcash_serialize(&mut buf))
            .expect("serialization into vec is infallible");

        hash.update(&buf);

        Some(hash.finalize())
    }

    fn sequence_hash(&self) -> Option<Hash> {
        if self.hash_type & SIGHASH_ANYONECANPAY == 0
            && (self.hash_type & SIGHASH_MASK) != SIGHASH_SINGLE
            && (self.hash_type & SIGHASH_MASK) != SIGHASH_NONE
        {
            return None;
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_PREVOUTS_HASH_PERSONALIZATION)
            .to_state();

        let mut buf = vec![];

        self.trans
            .inputs()
            .map(|input| match input {
                TransparentInput::PrevOut { sequence, .. } => sequence,
                TransparentInput::Coinbase { sequence, .. } => sequence,
            })
            .try_for_each(|sequence| (&mut buf).write_u32::<LittleEndian>(*sequence))
            .expect("serialization into vec is infallible");

        hash.update(&buf);

        Some(hash.finalize())
    }
}
