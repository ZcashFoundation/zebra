//! Transaction types.

use crate::types::Script;

use super::TransactionHash;

/// OutPoint
///
/// A particular transaction output reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutPoint {
    /// References the transaction that contains the UTXO being spent.
    pub hash: TransactionHash,

    /// Identifies which UTXO from that transaction is referenced; the
    /// first output is 0, etc.
    pub index: u32,
}

/// A transparent input to a transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparentInput {
    /// The previous output transaction reference.
    pub previous_output: OutPoint,

    /// Computational Script for confirming transaction authorization.
    pub signature_script: Script,

    /// Transaction version as defined by the sender. Intended for
    /// "replacement" of transactions when information is updated
    /// before inclusion into a block.
    pub sequence: u32,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparentOutput {
    /// Transaction value.
    // At https://en.bitcoin.it/wiki/Protocol_documentation#tx, this is an i64.
    // XXX refine to Amount ?
    pub value: u64,

    /// Usually contains the public key as a Bitcoin script setting up
    /// conditions to claim this output.
    pub pk_script: Script,
}
