//! Transaction references (draft, "Transaction References").
//!
//! Transactions are named in `get-tx` requests, announcements, and
//! `get-mempool` records by a 1-byte type followed by a type-dependent
//! identifier: a txid for version 1–4 transactions, a wtxid for version 5
//! and later, or a block-relative short ID for compact block reconstruction.

use std::io::{Read, Write};

use zebra_chain::{
    block,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
    transaction::{self, UnminedTxId, WtxId},
};

use super::{
    constants::{FULL_TX_ID_LEN, SHORT_TX_ID_LEN},
    wire::{read_u8, WireError},
};

/// The type byte of a [`TxRef::Txid`] reference.
pub const TX_REF_TXID: u8 = 0x01;

/// The type byte of a [`TxRef::Wtxid`] reference.
pub const TX_REF_WTXID: u8 = 0x02;

/// The type byte of a [`TxRef::ShortId`] reference.
pub const TX_REF_SHORTID: u8 = 0x03;

/// A short transaction ID: the low 6 bytes of a SipHash-2-4 of the
/// transaction's relay identifier, keyed from a compact block's header and
/// nonce (draft, "Short Transaction IDs").
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ShortTxId(pub [u8; SHORT_TX_ID_LEN]);

impl ZcashSerialize for ShortTxId {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_all(&self.0)
    }
}

impl ZcashDeserialize for ShortTxId {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; SHORT_TX_ID_LEN];
        reader.read_exact(&mut bytes)?;
        Ok(ShortTxId(bytes))
    }
}

/// A reference to a transaction.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TxRef {
    /// A transaction with version 4 or lower, by txid (`TXID`).
    Txid(transaction::Hash),

    /// A transaction with version 5 or higher, by wtxid (`WTXID`).
    Wtxid(WtxId),

    /// A transaction within a block, by the short ID it had in a compact
    /// block for that block (`SHORTID`). Only valid in `get-tx` requests.
    ShortId {
        /// The hash of the block the transaction is in.
        block: block::Hash,
        /// The short transaction ID, relative to the compact block the
        /// requester received for `block`.
        short_id: ShortTxId,
    },
}

impl TxRef {
    /// The encoded length of the largest reference type, in bytes.
    pub const MAX_ENCODED_LEN: usize = 1 + FULL_TX_ID_LEN;

    /// The unmined transaction ID this reference names, or `None` for a
    /// short ID, which only identifies a transaction relative to a block.
    pub fn unmined_id(&self) -> Option<UnminedTxId> {
        match *self {
            TxRef::Txid(hash) => Some(UnminedTxId::Legacy(hash)),
            TxRef::Wtxid(wtx_id) => Some(UnminedTxId::Witnessed(wtx_id)),
            TxRef::ShortId { .. } => None,
        }
    }

    /// Writes the reference in its wire encoding.
    pub fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        match self {
            TxRef::Txid(hash) => {
                writer.write_all(&[TX_REF_TXID])?;
                hash.zcash_serialize(writer)?;
            }
            TxRef::Wtxid(wtx_id) => {
                writer.write_all(&[TX_REF_WTXID])?;
                wtx_id.zcash_serialize(writer)?;
            }
            TxRef::ShortId { block, short_id } => {
                writer.write_all(&[TX_REF_SHORTID])?;
                block.zcash_serialize(&mut writer)?;
                short_id.zcash_serialize(writer)?;
            }
        }
        Ok(())
    }

    /// Reads one reference from `reader`.
    ///
    /// An unknown type byte is a `PROTOCOL_ERROR`.
    pub(crate) fn read(reader: &mut &[u8]) -> Result<Self, WireError> {
        Ok(match read_u8(reader, "transaction reference type")? {
            TX_REF_TXID => TxRef::Txid(
                transaction::Hash::zcash_deserialize(&mut *reader)
                    .map_err(WireError::malformed("txid reference"))?,
            ),
            TX_REF_WTXID => TxRef::Wtxid(
                WtxId::zcash_deserialize(&mut *reader)
                    .map_err(WireError::malformed("wtxid reference"))?,
            ),
            TX_REF_SHORTID => TxRef::ShortId {
                block: block::Hash::zcash_deserialize(&mut *reader)
                    .map_err(WireError::malformed("short ID reference block hash"))?,
                short_id: ShortTxId::zcash_deserialize(&mut *reader)
                    .map_err(WireError::malformed("short ID"))?,
            },
            value => {
                return Err(WireError::UnknownByte {
                    what: "transaction reference type",
                    value,
                })
            }
        })
    }
}

impl From<UnminedTxId> for TxRef {
    /// A legacy ID is referenced by txid, a witnessed ID by wtxid.
    fn from(id: UnminedTxId) -> Self {
        match id {
            UnminedTxId::Legacy(hash) => TxRef::Txid(hash),
            UnminedTxId::Witnessed(wtx_id) => TxRef::Wtxid(wtx_id),
        }
    }
}
