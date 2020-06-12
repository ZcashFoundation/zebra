//! Transaction types.
#![allow(clippy::unit_arg)]

#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::types::{BlockHeight, Script};

use super::TransactionHash;

/// Arbitrary data inserted by miners into a coinbase transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoinbaseData(
    /// Invariant: this vec, together with the coinbase height, must be less than
    /// 100 bytes. We enforce this by only constructing CoinbaseData fields by
    /// parsing blocks with 100-byte data fields. When we implement block
    /// creation, we should provide a constructor for the coinbase data field
    /// that restricts it to 95 = 100 -1 -4 bytes (safe for any block height up
    /// to 500_000_000).
    pub(super) Vec<u8>,
);

impl AsRef<[u8]> for CoinbaseData {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

/// OutPoint
///
/// A particular transaction output reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct OutPoint {
    /// References the transaction that contains the UTXO being spent.
    pub hash: TransactionHash,

    /// Identifies which UTXO from that transaction is referenced; the
    /// first output is 0, etc.
    pub index: u32,
}

/// A transparent input to a transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransparentInput {
    /// A reference to an output of a previous transaction.
    PrevOut {
        /// The previous output transaction reference.
        outpoint: OutPoint,
        /// The script that authorizes spending `outpoint`.
        script: Script,
        /// The sequence number for the output.
        sequence: u32,
    },
    /// New coins created by the block reward.
    Coinbase {
        /// The height of this block.
        height: BlockHeight,
        /// Free data inserted by miners after the block height.
        data: CoinbaseData,
        /// The sequence number for the output.
        sequence: u32,
    },
}

/// A transparent output from a transaction.
///
/// The most fundamental building block of a transaction is a
/// transaction output -- the ZEC you own in your "wallet" is in
/// fact a subset of unspent transaction outputs (or "UTXO"s) of the
/// global UTXO set.
///
/// UTXOs are indivisible, discrete units of value which can only be
/// consumed in their entirety. Thus, if I want to send you 1 ZEC and
/// I only own one UTXO worth 2 ZEC, I would construct a transaction
/// that spends my UTXO and sends 1 ZEC to you and 1 ZEC back to me
/// (just like receiving change).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct TransparentOutput {
    /// Transaction value.
    // At https://en.bitcoin.it/wiki/Protocol_documentation#tx, this is an i64.
    // XXX refine to Amount ?
    pub value: u64,

    /// Usually contains the public key as a Bitcoin script setting up
    /// conditions to claim this output.
    pub pk_script: Script,
}
