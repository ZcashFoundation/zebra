//! Transaction types.

mod hash;
mod joinsplit;
mod serialize;
mod shielded_data;
mod transparent;

#[cfg(test)]
mod test_vectors;
#[cfg(test)]
mod tests;

pub use hash::TransactionHash;
pub use joinsplit::{JoinSplit, JoinSplitData};
pub use shielded_data::{OutputDescription, ShieldedData, SpendDescription};
pub use transparent::{OutPoint, TransparentInput, TransparentOutput};

use crate::proofs::{Bctv14Proof, Groth16Proof};
use crate::types::{BlockHeight, LockTime};

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
#[derive(Clone, Debug, PartialEq, Eq)]
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
        // XXX refine this to an Amount type.
        value_balance: i64,
        /// The shielded data for this transaction, if any.
        shielded_data: Option<ShieldedData>,
        /// The JoinSplit data for this transaction, if any.
        joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
    },
}
