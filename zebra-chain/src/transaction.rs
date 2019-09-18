//! Transaction types.

/// OutPoint
///
/// A particular transaction output reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutPoint {
    /// The hash of the referenced transaction.
    pub hash: [u8; 32],

    /// The index of the specific output in the transaction. The first output is 0, etc.
    pub index: u32,
}

/// Transaction Input
// `Copy` cannot be implemented for `Vec<u8>`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionInput {
    /// The previous output transaction reference.
    pub previous_output: OutPoint,

    /// Computational Script for confirming transaction authorization.
    // XXX pzec uses their own `Bytes` type that wraps a `Vec<u8>`
    // with some extra methods.
    pub signature_script: Vec<u8>,

    /// Transaction version as defined by the sender. Intended for
    /// "replacement" of transactions when information is updated
    /// before inclusion into a block.
    pub sequence: u32,
}

/// Transaction Output
// `Copy` cannot be implemented for `Vec<u8>`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionOutput {
    /// Transaction value.
    // At https://en.bitcoin.it/wiki/Protocol_documentation#tx, this is an i64.
    pub value: u64,

    /// Usually contains the public key as a Bitcoin script setting up
    /// conditions to claim this output.
    pub pk_script: Vec<u8>,
}

/// Transaction Input
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transaction {
    /// Transaction data format version (note, this is signed).
    pub version: i32,

    /// A list of 1 or more transaction inputs or sources for coins.
    pub tx_in: Vec<TransactionInput>,

    /// A list of 1 or more transaction outputs or destinations for coins.
    pub tx_out: Vec<TransactionOutput>,

    /// The block number or timestamp at which this transaction is unlocked:
    ///
    /// |Value       |Description                                         |
    /// |------------|----------------------------------------------------|
    /// |0           |Not locked (default)                                |
    /// |< 500000000 |Block number at which this transaction is unlocked  |
    /// |>= 500000000|UNIX timestamp at which this transaction is unlocked|
    ///
    /// If all `TransactionInput`s have final (0xffffffff) sequence
    /// numbers, then lock_time is irrelevant. Otherwise, the
    /// transaction may not be added to a block until after `lock_time`.
    pub lock_time: u32,
}
