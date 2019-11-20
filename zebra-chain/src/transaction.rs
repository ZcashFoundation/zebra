//! Transaction types.

use std::io;

use crate::{
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
    sha256d_writer::Sha256dWriter,
    types::{BlockHeight, LockTime, Script},
};

/// A hash of a `Transaction`
///
/// TODO: I'm pretty sure this is also a SHA256d hash but I haven't
/// confirmed it yet.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TransactionHash(pub [u8; 32]);

impl From<Transaction> for TransactionHash {
    fn from(transaction: Transaction) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        transaction
            .zcash_serialize(&mut hash_writer)
            .expect("Transactions must serialize into the hash.");
        Self(hash_writer.finish())
    }
}

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

/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendDescription {}
/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDescription {}

/// Sapling-on-Groth16 spend and output descriptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShieldedData {
    /// A sequence of spend descriptions for this transaction.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#spendencoding
    pub shielded_spends: Vec<SpendDescription>,
    /// A sequence of shielded outputs for this transaction.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#outputencoding
    pub shielded_outputs: Vec<OutputDescription>,
    /// A signature on the transaction hash.
    // XXX refine this type to a RedJubjub signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub binding_sig: [u64; 8],
}

/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitBctv14 {}
/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitGroth16 {}

/// Pre-Sapling JoinSplit data using Sprout-on-BCTV14 proofs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyJoinSplitData {
    /// A sequence of JoinSplit descriptions using BCTV14 proofs.
    pub joinsplits: Vec<JoinSplitBctv14>,
    /// The public key for the JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 pubkey.
    pub pub_key: [u8; 32],
    /// The JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub sig: [u64; 8],
}

/// Post-Sapling JoinSplit data using Sprout-on-Groth16 proofs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaplingJoinSplitData {
    /// A sequence of JoinSplit descriptions using Groth16 proofs.
    pub joinsplits: Vec<JoinSplitGroth16>,
    /// The public key for the JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 pubkey.
    pub pub_key: [u8; 32],
    /// The JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub sig: [u64; 8],
}

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
        joinsplit_data: Option<LegacyJoinSplitData>,
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
        joinsplit_data: Option<LegacyJoinSplitData>,
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
        joinsplit_data: Option<SaplingJoinSplitData>,
    },
}

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl ZcashDeserialize for Transaction {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
