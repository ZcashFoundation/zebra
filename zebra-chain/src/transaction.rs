//! Transaction types.

use serde::{Deserialize, Serialize};

mod hash;
mod joinsplit;
mod serialize;
mod shielded_data;
mod sighash;
mod transparent;

#[cfg(test)]
mod tests;

pub use hash::TransactionHash;
pub use joinsplit::{JoinSplit, JoinSplitData};
pub use shielded_data::{Output, ShieldedData, Spend};
pub use transparent::{CoinbaseData, OutPoint, TransparentInput, TransparentOutput};

use crate::proofs::{Bctv14Proof, Groth16Proof};
use crate::{
    types::{amount::Amount, BlockHeight, LockTime},
    Network,
};
use blake2b_simd::Hash;

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
    /// Access the transparent inputs of this transaction, regardless of version.
    pub fn inputs(&self) -> &[TransparentInput] {
        match self {
            Transaction::V1 { ref inputs, .. } => inputs,
            Transaction::V2 { ref inputs, .. } => inputs,
            Transaction::V3 { ref inputs, .. } => inputs,
            Transaction::V4 { ref inputs, .. } => inputs,
        }
    }

    /// Access the transparent outputs of this transaction, regardless of version.
    pub fn outputs(&self) -> &[TransparentOutput] {
        match self {
            Transaction::V1 { ref outputs, .. } => outputs,
            Transaction::V2 { ref outputs, .. } => outputs,
            Transaction::V3 { ref outputs, .. } => outputs,
            Transaction::V4 { ref outputs, .. } => outputs,
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
            .iter()
            .any(|input| matches!(input, TransparentInput::Coinbase { .. }))
    }

    /// Returns `true` if this transaction is a coinbase transaction.
    pub fn is_coinbase(&self) -> bool {
        self.inputs().len() == 1
            && matches!(
                self.inputs().get(0),
                Some(TransparentInput::Coinbase { .. })
            )
    }

    // TODO(jlusby): refine type
    pub fn sighash(&self, network: Network, height: BlockHeight, hash_type: u32) -> Hash {
        sighash::SigHasher {
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
