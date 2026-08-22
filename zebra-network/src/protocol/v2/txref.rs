//! Transaction references for the version 2 Zcash P2P network protocol.
//!
//! Transactions are identified in announcements and requests by *transaction
//! references*. Per ZIP 239, transactions with version 5 or later are relayed
//! by wtxid, and transactions with version 4 or earlier by txid.

use std::io;

use tokio::io::AsyncRead;

use zebra_chain::{
    block,
    transaction::{self, UnminedTxId, WtxId},
};

use super::{compact_block::ShortTxId, record, types::WireError};

/// The wire type byte of a `TXID` transaction reference.
pub const TXREF_TYPE_TXID: u8 = 0x01;

/// The wire type byte of a `WTXID` transaction reference.
pub const TXREF_TYPE_WTXID: u8 = 0x02;

/// The wire type byte of a `SHORTID` transaction reference.
pub const TXREF_TYPE_SHORTID: u8 = 0x03;

/// A reference to a transaction, in an announcement or request.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TransactionReference {
    /// A transaction with version 4 or earlier, identified by its txid.
    Txid(transaction::Hash),

    /// A transaction with version 5 or later, identified by its wtxid
    /// (the txid followed by the authorizing data commitment).
    Wtxid(WtxId),

    /// A transaction within a specific block, identified by the block hash
    /// and a short transaction ID.
    ///
    /// Only valid in `get-tx` requests.
    ShortId {
        /// The hash of the block containing the transaction.
        block_hash: block::Hash,

        /// The short transaction ID, relative to the compact block most
        /// recently sent by the responder for this block.
        short_id: ShortTxId,
    },
}

impl TransactionReference {
    /// Encodes this transaction reference to `writer`.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        match self {
            TransactionReference::Txid(txid) => {
                writer.write_all(&[TXREF_TYPE_TXID])?;
                writer.write_all(&txid.0)?;
            }
            TransactionReference::Wtxid(wtxid) => {
                writer.write_all(&[TXREF_TYPE_WTXID])?;
                writer.write_all(&wtxid.as_bytes())?;
            }
            TransactionReference::ShortId {
                block_hash,
                short_id,
            } => {
                writer.write_all(&[TXREF_TYPE_SHORTID])?;
                writer.write_all(&block_hash.0)?;
                writer.write_all(&short_id.0)?;
            }
        }

        Ok(())
    }

    /// Reads a transaction reference from `reader`.
    ///
    /// A transaction reference with an unrecognized type is a connection
    /// error of type `PROTOCOL_ERROR`.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let ref_type = record::read_u8(reader).await?;

        match ref_type {
            TXREF_TYPE_TXID => {
                let mut txid = [0u8; 32];
                record::read_exact_or_incomplete(reader, &mut txid).await?;
                Ok(TransactionReference::Txid(txid.into()))
            }
            TXREF_TYPE_WTXID => {
                let mut wtxid = [0u8; 64];
                record::read_exact_or_incomplete(reader, &mut wtxid).await?;
                Ok(TransactionReference::Wtxid(wtxid.into()))
            }
            TXREF_TYPE_SHORTID => {
                let block_hash = record::read_block_hash(reader).await?;
                let mut short_id = [0u8; 6];
                record::read_exact_or_incomplete(reader, &mut short_id).await?;
                Ok(TransactionReference::ShortId {
                    block_hash,
                    short_id: ShortTxId(short_id),
                })
            }
            unknown => Err(WireError::Protocol(format!(
                "unrecognized transaction reference type {unknown:#04x}",
            ))),
        }
    }

    /// Parses a transaction reference from a complete buffer, rejecting
    /// trailing data.
    ///
    /// Used for transaction announcement records, which each contain exactly
    /// one reference.
    pub async fn parse_exact(payload: &[u8]) -> Result<Self, WireError> {
        let mut reader = payload;
        let reference = Self::read(&mut reader).await?;

        if !reader.is_empty() {
            return Err(WireError::Protocol(
                "trailing data after a transaction reference".to_string(),
            ));
        }

        Ok(reference)
    }

    /// Returns true if this is a `SHORTID` reference.
    ///
    /// `SHORTID` references are only valid in `get-tx` requests: they must
    /// not be used in announcements or `get-mempool` responses.
    pub fn is_short_id(&self) -> bool {
        matches!(self, TransactionReference::ShortId { .. })
    }
}

impl From<UnminedTxId> for TransactionReference {
    /// Converts a mempool transaction ID to the reference used to relay it,
    /// following ZIP 239: legacy IDs identify transactions with version 4 or
    /// earlier, witnessed IDs identify transactions with version 5 or later.
    fn from(id: UnminedTxId) -> Self {
        match id {
            UnminedTxId::Legacy(txid) => TransactionReference::Txid(txid),
            UnminedTxId::Witnessed(wtxid) => TransactionReference::Wtxid(wtxid),
        }
    }
}

impl TryFrom<TransactionReference> for UnminedTxId {
    type Error = WireError;

    /// Converts a `TXID` or `WTXID` reference to a mempool transaction ID.
    ///
    /// `SHORTID` references do not identify a transaction on their own, so
    /// they have no [`UnminedTxId`] equivalent.
    fn try_from(reference: TransactionReference) -> Result<Self, Self::Error> {
        match reference {
            TransactionReference::Txid(txid) => Ok(UnminedTxId::Legacy(txid)),
            TransactionReference::Wtxid(wtxid) => Ok(UnminedTxId::Witnessed(wtxid)),
            TransactionReference::ShortId { .. } => Err(WireError::Protocol(
                "a SHORTID reference is not valid here".to_string(),
            )),
        }
    }
}
