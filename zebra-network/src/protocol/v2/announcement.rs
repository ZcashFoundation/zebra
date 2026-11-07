//! Records on the announcement stream types and the `get-mempool`
//! response (draft, "Announcement Stream Types", "`get-mempool`").
//!
//! Each announcement stream carries [records](crate::protocol::v2::record)
//! of one kind, decoded here from their complete payloads; a `get-mempool`
//! response is an open-ended sequence of records with the transaction
//! announcement layout. A record that does not conform to its stream type's
//! format is a connection error of type `PROTOCOL_ERROR` (draft,
//! "Announcement Streams").

use std::sync::Arc;

use bytes::Bytes;

use zebra_chain::{block, transaction::UnminedTxId};

use crate::{meta_addr::MetaAddr, protocol::external::AddrV2};

use super::{
    txref::TxRef,
    wire::{deserialize_exact, ensure_consumed, WireError},
};

/// The `kind` byte of a header announcement.
pub const BLOCK_ANNOUNCEMENT_HEADER: u8 = 0x00;

/// The `kind` byte of a compact block announcement.
pub const BLOCK_ANNOUNCEMENT_COMPACT: u8 = 0x01;

/// Decodes a block announcement record into its header (draft, "Block Announcements").
///
/// This node sets `announce = 0`, so a compact block (`kind = 0x01`) is a record the peer must not
/// send: it is rejected from its kind byte alone, without parsing the body.
pub fn decode_block_announcement(payload: &[u8]) -> Result<Arc<block::Header>, WireError> {
    let Some((&kind, header)) = payload.split_first() else {
        return Err(WireError::Truncated {
            what: "block announcement kind",
        });
    };
    match kind {
        // The header is the rest of the payload, with no length prefix.
        BLOCK_ANNOUNCEMENT_HEADER => deserialize_exact(header, "announced header"),
        BLOCK_ANNOUNCEMENT_COMPACT => Err(WireError::Nonconforming {
            what: "block announcement",
            reason: "compact block announcement to a peer whose init record had announce = 0",
        }),
        value => Err(WireError::UnknownByte {
            what: "block announcement kind",
            value,
        }),
    }
}

/// Encodes a transaction announcement record, or a `get-mempool` response
/// record: a `TXID` or `WTXID` reference (draft, "Transaction
/// Announcements", "`get-mempool`"); a short ID cannot occur, an
/// `UnminedTxId` is always one of the two.
pub fn encode_tx_record(id: UnminedTxId) -> Bytes {
    let mut payload = Vec::with_capacity(TxRef::MAX_ENCODED_LEN);
    TxRef::from(id)
        .write(&mut payload)
        .expect("writing a fixed-size reference into an in-memory Vec never fails");
    payload.into()
}

/// Decodes a transaction announcement record, or a `get-mempool` response
/// record, into its `TXID` or `WTXID` reference.
///
/// A `SHORTID` reference is a nonconforming record.
pub fn decode_tx_record(payload: &[u8]) -> Result<TxRef, WireError> {
    let mut reader = payload;
    let txref = TxRef::read(&mut reader)?;
    ensure_consumed(reader, "transaction announcement")?;

    if matches!(txref, TxRef::ShortId { .. }) {
        return Err(WireError::Nonconforming {
            what: "transaction announcement",
            reason: "short ID references are not allowed in announcements",
        });
    }
    Ok(txref)
}

/// Decodes an address announcement record into a gossiped address, or
/// `None` for a well-formed record with a network ID Zebra does not support.
pub fn decode_addr_record(payload: &[u8]) -> Result<Option<MetaAddr>, WireError> {
    let record = deserialize_exact::<AddrV2>(payload, "address announcement")?;
    Ok(MetaAddr::try_from(record).ok())
}
