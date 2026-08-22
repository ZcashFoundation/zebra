//! Compact block encoding for the version 2 Zcash P2P network protocol.
//!
//! Compact block relay is adapted from BIP 152: a block is relayed as its
//! header plus identifiers of its transactions — short transaction IDs, or
//! full transaction IDs at the receiver's option — with transactions the
//! receiver is predicted not to have prefilled in full.

use std::{io, sync::Arc};

use sha2::{Digest, Sha256};
use siphasher::sip::SipHasher24;
use tokio::io::AsyncRead;

use zebra_chain::{
    block::{merkle::AUTH_DIGEST_PLACEHOLDER, Block, Header},
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{Transaction, UnminedTxId, WtxId},
};

use super::{
    constants::{MAX_COMPACT_BLOCK_TX_COUNT, MAX_PREFILLED_TX_INDEX, MAX_RECORD_PAYLOAD_LEN},
    record,
    types::WireError,
};

/// The `ids_kind` byte of a compact block whose `ids` field contains short
/// transaction IDs.
pub const IDS_KIND_SHORT: u8 = 0x00;

/// The `ids_kind` byte of a compact block whose `ids` field contains full
/// transaction IDs.
pub const IDS_KIND_FULL: u8 = 0x01;

/// A short transaction ID: the least significant 6 bytes, in little-endian
/// byte order, of a keyed SipHash-2-4 of the transaction's relay identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ShortTxId(pub [u8; 6]);

/// The transaction IDs of a compact block: short or full, at the receiver's
/// option.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactBlockIds {
    /// Short transaction IDs (`ids_kind = 0`).
    Short(Vec<ShortTxId>),

    /// Full transaction IDs (`ids_kind = 1`): each is the transaction's txid
    /// followed by its authorizing data commitment, with the ZIP 244
    /// placeholder for transactions with version 4 or earlier.
    Full(Vec<WtxId>),
}

/// A transaction included in full in a compact block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrefilledTransaction {
    /// The absolute index of this transaction within the block.
    ///
    /// On the wire, indexes are differentially encoded; this is the decoded
    /// absolute index.
    pub index: u64,

    /// The transaction.
    pub tx: Arc<Transaction>,
}

/// A compact block: a block's header plus a compact representation of its
/// transactions.
///
/// Each transaction in the block, in block order, is represented either by a
/// transaction ID in `ids` or by a full serialized transaction in
/// `prefilled`. The coinbase transaction must be prefilled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactBlock {
    /// The block header.
    pub header: Arc<Header>,

    /// The nonce for short transaction ID computation.
    ///
    /// If `ids` contains full transaction IDs, this field should be 0 and
    /// must be ignored.
    pub nonce: u64,

    /// The block's transaction IDs, in block order, excluding prefilled
    /// transactions.
    pub ids: CompactBlockIds,

    /// The prefilled transactions, with strictly increasing absolute
    /// indexes.
    pub prefilled: Vec<PrefilledTransaction>,
}

impl CompactBlock {
    /// Builds a compact block for `block`.
    ///
    /// The coinbase transaction is always prefilled; `extra_prefilled`
    /// selects additional transaction indexes to prefill (transactions the
    /// sender predicts the receiver does not have). If `full_ids` is true,
    /// the remaining transactions are identified by full transaction IDs and
    /// the nonce is 0; otherwise they are identified by short transaction
    /// IDs computed with `nonce`.
    #[allow(clippy::unwrap_in_result)]
    pub fn from_block(
        block: &Block,
        nonce: u64,
        full_ids: bool,
        extra_prefilled: &[u64],
    ) -> Result<Self, WireError> {
        let header = block.header.clone();
        let tx_count = block.transactions.len() as u64;

        if tx_count > MAX_COMPACT_BLOCK_TX_COUNT
            || tx_count.saturating_sub(1) > MAX_PREFILLED_TX_INDEX
        {
            return Err(WireError::Local(format!(
                "block with {tx_count} transactions cannot be relayed as a compact block",
            )));
        }

        let mut prefilled = Vec::new();
        let mut id_txs = Vec::new();

        for (index, tx) in block.transactions.iter().enumerate() {
            let index = index as u64;
            // The coinbase transaction (index 0) must be prefilled.
            if index == 0 || extra_prefilled.contains(&index) {
                prefilled.push(PrefilledTransaction {
                    index,
                    tx: tx.clone(),
                });
            } else {
                id_txs.push(tx);
            }
        }

        let ids = if full_ids {
            CompactBlockIds::Full(
                id_txs
                    .into_iter()
                    .map(|tx| full_transaction_id(&UnminedTxId::from(tx.as_ref())))
                    .collect(),
            )
        } else {
            let header_bytes = header
                .zcash_serialize_to_vec()
                .expect("serializing a header to a Vec never fails");
            let (k0, k1) = short_id_keys(&header_bytes, nonce);

            CompactBlockIds::Short(
                id_txs
                    .into_iter()
                    .map(|tx| short_transaction_id(k0, k1, &UnminedTxId::from(tx.as_ref())))
                    .collect(),
            )
        };

        Ok(CompactBlock {
            header,
            nonce: if full_ids { 0 } else { nonce },
            ids,
            prefilled,
        })
    }

    /// Encodes this compact block to `writer`.
    #[allow(clippy::unwrap_in_result)]
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        let header_bytes = self
            .header
            .zcash_serialize_to_vec()
            .expect("serializing a header to a Vec never fails");
        record::write_record(&mut writer, &header_bytes)?;

        writer.write_all(&self.nonce.to_le_bytes())?;

        match &self.ids {
            CompactBlockIds::Short(ids) => {
                writer.write_all(&[IDS_KIND_SHORT])?;
                record::write_compact_size(&mut writer, ids.len() as u64)?;
                for id in ids {
                    writer.write_all(&id.0)?;
                }
            }
            CompactBlockIds::Full(ids) => {
                writer.write_all(&[IDS_KIND_FULL])?;
                record::write_compact_size(&mut writer, ids.len() as u64)?;
                for id in ids {
                    writer.write_all(&id.as_bytes())?;
                }
            }
        }

        record::write_compact_size(&mut writer, self.prefilled.len() as u64)?;
        let mut previous_index: Option<u64> = None;
        for prefilled in &self.prefilled {
            let differential = match previous_index {
                None => prefilled.index,
                Some(previous) => prefilled
                    .index
                    .checked_sub(previous + 1)
                    .expect("prefilled indexes are strictly increasing"),
            };
            previous_index = Some(prefilled.index);

            record::write_compact_size(&mut writer, differential)?;

            let tx_bytes = prefilled
                .tx
                .zcash_serialize_to_vec()
                .expect("serializing a transaction to a Vec never fails");
            record::write_record(&mut writer, &tx_bytes)?;
        }

        Ok(())
    }

    /// Reads a compact block from `reader`.
    ///
    /// A compact block in which `ids_count + prefilled_count` exceeds
    /// [`MAX_COMPACT_BLOCK_TX_COUNT`] is a connection error of type `FLOOD`.
    /// A compact block with an invalid `ids_kind`, or in which any prefilled
    /// absolute index would exceed [`MAX_PREFILLED_TX_INDEX`], overflows, or
    /// is not strictly increasing, is rejected as a `PROTOCOL_ERROR`.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let header_bytes =
            record::read_length_prefixed_bytes(reader, MAX_RECORD_PAYLOAD_LEN).await?;
        let header = Arc::new(Header::zcash_deserialize(header_bytes.as_slice())?);

        let nonce = record::read_exact_u64_le(reader).await?;

        let ids_kind = record::read_u8(reader).await?;
        let ids_count = record::read_compact_size(reader).await?;

        if ids_count > MAX_COMPACT_BLOCK_TX_COUNT {
            return Err(WireError::Flood(format!(
                "compact block ids_count {ids_count} exceeds the transaction count limit",
            )));
        }

        let ids = match ids_kind {
            IDS_KIND_SHORT => {
                let mut ids = Vec::with_capacity(record::preallocate_len(ids_count));
                for _ in 0..ids_count {
                    let mut id = [0u8; 6];
                    record::read_exact_or_incomplete(reader, &mut id).await?;
                    ids.push(ShortTxId(id));
                }
                CompactBlockIds::Short(ids)
            }
            IDS_KIND_FULL => {
                let mut ids = Vec::with_capacity(record::preallocate_len(ids_count));
                for _ in 0..ids_count {
                    let mut id = [0u8; 64];
                    record::read_exact_or_incomplete(reader, &mut id).await?;
                    ids.push(WtxId::from(id));
                }
                CompactBlockIds::Full(ids)
            }
            invalid => {
                return Err(WireError::Protocol(format!(
                    "invalid compact block ids_kind {invalid}: must be 0 or 1",
                )))
            }
        };

        let prefilled_count = record::read_compact_size(reader).await?;
        let total_count = ids_count.checked_add(prefilled_count);
        if total_count.is_none_or(|total| total > MAX_COMPACT_BLOCK_TX_COUNT) {
            return Err(WireError::Flood(format!(
                "compact block transaction count {ids_count} + {prefilled_count} \
                 exceeds the limit {MAX_COMPACT_BLOCK_TX_COUNT}",
            )));
        }

        let mut prefilled = Vec::with_capacity(record::preallocate_len(prefilled_count));
        let mut previous_index: Option<u64> = None;
        for _ in 0..prefilled_count {
            let differential = record::read_compact_size(reader).await?;

            let index = match previous_index {
                None => differential,
                Some(previous) => previous
                    .checked_add(1)
                    .and_then(|next| next.checked_add(differential))
                    .ok_or_else(|| {
                        WireError::Protocol("prefilled transaction index overflow".to_string())
                    })?,
            };

            if index > MAX_PREFILLED_TX_INDEX {
                return Err(WireError::Protocol(format!(
                    "prefilled transaction index {index} exceeds the limit",
                )));
            }
            previous_index = Some(index);

            let tx_bytes =
                record::read_length_prefixed_bytes(reader, MAX_RECORD_PAYLOAD_LEN).await?;
            let tx = Arc::new(Transaction::zcash_deserialize(tx_bytes.as_slice())?);

            prefilled.push(PrefilledTransaction { index, tx });
        }

        Ok(CompactBlock {
            header,
            nonce,
            ids,
            prefilled,
        })
    }
}

/// Computes the SipHash keys for short transaction IDs: the single SHA-256
/// hash of the serialized block header followed by the 8-byte little-endian
/// nonce, interpreted as two little-endian `u64` values.
pub fn short_id_keys(serialized_header: &[u8], nonce: u64) -> (u64, u64) {
    let mut hasher = Sha256::new();
    hasher.update(serialized_header);
    hasher.update(nonce.to_le_bytes());
    let hash = hasher.finalize();

    let k0 = u64::from_le_bytes(hash[0..8].try_into().expect("slice length is 8"));
    let k1 = u64::from_le_bytes(hash[8..16].try_into().expect("slice length is 8"));

    (k0, k1)
}

/// Computes the short transaction ID of a transaction: the least significant
/// 6 bytes, in little-endian byte order, of `SipHash-2-4(k0, k1, input)`,
/// where the input is the transaction's relay identifier.
pub fn short_transaction_id(k0: u64, k1: u64, id: &UnminedTxId) -> ShortTxId {
    use std::hash::Hasher;

    let mut hasher = SipHasher24::new_with_keys(k0, k1);
    match id {
        // For a transaction with version 4 or earlier, the input is its
        // 32-byte txid.
        UnminedTxId::Legacy(txid) => hasher.write(&txid.0),
        // For a transaction with version 5 or later, the input is its
        // 64-byte wtxid.
        UnminedTxId::Witnessed(wtxid) => hasher.write(&wtxid.as_bytes()),
    }

    let hash = hasher.finish();
    let bytes = hash.to_le_bytes();

    ShortTxId(bytes[0..6].try_into().expect("slice length is 6"))
}

/// Returns the full transaction ID of a transaction: its txid followed by
/// its authorizing data commitment, with the ZIP 244 placeholder for
/// transactions with version 4 or earlier.
pub fn full_transaction_id(id: &UnminedTxId) -> WtxId {
    WtxId {
        id: id.mined_id(),
        auth_digest: id.auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER),
    }
}
