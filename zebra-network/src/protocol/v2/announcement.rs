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

use zebra_chain::{
    block,
    serialization::{DateTime32, Duration32, ZcashSerialize},
    transaction::UnminedTxId,
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{canonical_peer_addr, AddrV2},
};

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

/// Encodes an address announcement record: one `addrv2` record, the layout
/// of one `get-addr` response entry (draft, "Address Announcements").
///
/// The address must have its services and last-seen time, as gossiped and
/// sanitized addresses do; one without is a local error.
pub fn encode_addr_record(addr: &MetaAddr) -> Result<Bytes, WireError> {
    let unencodable = |reason| WireError::LocalUnencodable {
        what: "address record",
        reason,
    };
    let untrusted_services = addr
        .services()
        .ok_or(unencodable("address has no services"))?;
    let untrusted_last_seen = addr
        .last_seen()
        .ok_or(unencodable("address has no last seen time"))?;
    let record = AddrV2::IpAddr {
        untrusted_last_seen,
        untrusted_services,
        addr: canonical_peer_addr(addr.addr()),
    };
    let mut payload = Vec::new();
    record.zcash_serialize(&mut payload)?;
    Ok(payload.into())
}

/// How far in the future a relayed address record's `time` may be before it is replaced (draft,
/// "Address Records").
pub const MAX_RELAYED_TIME_SKEW: Duration32 = Duration32::from_minutes(10);

/// How far in the past a replaced relayed `time` is put (draft, "Address Records").
pub const CLAMPED_RELAYED_TIME_AGE: Duration32 = Duration32::from_days(5);

/// Clamps the `time` of a relayed address record at `now` (draft, "Address Records"): a `time` of
/// zero, or more than [`MAX_RELAYED_TIME_SKEW`] in the future, becomes `now` minus
/// [`CLAMPED_RELAYED_TIME_AGE`]. Any other `time` is kept.
pub fn clamp_relayed_time(time: DateTime32, now: DateTime32) -> DateTime32 {
    if time == DateTime32::MIN || time > now.saturating_add(MAX_RELAYED_TIME_SKEW) {
        now.saturating_sub(CLAMPED_RELAYED_TIME_AGE)
    } else {
        time
    }
}
