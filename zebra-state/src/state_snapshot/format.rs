//! The state snapshot format (design, "Formats"): the manifest that names a
//! snapshot's components and pieces, and the piece records that carry the
//! column family rows.
//!
//! Integers are little-endian, counts and lengths inside pieces are canonical
//! CompactSizes (ZIP 204), and hashes are SHA-256. Decoding is strict: every
//! bound is checked before anything is allocated from it, every value the
//! format does not define is an error, and so are trailing bytes.
//!
//! # Manifest
//!
//! ```text
//! magic "ZEBRA-STATE-SNAPSHOT" ‖ snapshot_format u16 (2)
//! genesis_hash 32 ‖ height u32 ‖ block_hash 32 ‖ window_start u32 ‖ db_format_major u16 ‖ index_mode u8 (0)
//! [Regtest and custom testnets only: parameters_hash 32]
//! component_count u16 (28), then each component in [`ComponentId`] order:
//!   component_id u16 ‖ entry_count u64 ‖ piece_count u32 ‖ pieces[ size u32 ‖ sha256 32 ]
//! ```
//!
//! # Piece
//!
//! A piece is a sequence of records from one component, each
//! `CompactSize(key_len) ‖ key ‖ CompactSize(value_len) ‖ value`, with strictly
//! increasing keys, at most [`MAX_PIECE_BYTES`] long. An empty component has
//! no pieces, and a single record over the limit cannot be exported.

use std::{
    fmt,
    io::{self, Read, Write},
};

use thiserror::Error;

use zebra_chain::{
    block,
    parameters::Network,
    serialization::{CompactSize64, SerializationError, ZcashDeserialize, ZcashSerialize},
    state_snapshot::ObjectHash,
};

mod canonical;

#[cfg(test)]
mod tests;

pub use canonical::canonical_value;

/// The string every manifest starts with.
pub const MAGIC: &[u8; 20] = b"ZEBRA-STATE-SNAPSHOT";

/// The number of block hashes, headers and block infos a snapshot ships,
/// ending at its height: the headers window, which gives the block after the
/// snapshot its difficulty and time context, and `getblockheader` its answers
/// (design, "Snapshot contents"). Only the snapshot block's body is shipped
/// with them.
pub const WINDOW_LEN: u32 = 2304;

/// Returns the first height of the headers window that a snapshot at `height`
/// ships: `L = max(1, H - 2303)`. Genesis is shipped as well.
pub fn window_start(height: block::Height) -> block::Height {
    block::Height(height.0.saturating_sub(WINDOW_LEN - 1).max(1))
}

/// The snapshot format this code reads and writes. It changes only when the
/// shipped components or encodings change; the importer maps it onto the
/// running database format.
///
/// Format 2 ships the bodies of genesis and the snapshot block only, instead of
/// the whole window's.
pub const SNAPSHOT_FORMAT: u16 = 2;

/// The largest manifest, 1 MiB: a manifest is fetched whole before it is
/// decoded, so this bounds the download and the decode.
pub const MAX_MANIFEST_BYTES: usize = 1 << 20;

/// The largest piece, 16 MiB. Pieces are split at record boundaries, so a
/// single record over this limit cannot be exported.
pub const MAX_PIECE_BYTES: usize = 16 << 20;

/// The index mode of a snapshot with the default indexes: no spending index.
pub const INDEX_MODE_DEFAULT: u8 = 0;

/// The encoded length of one piece entry in a manifest: the size and the hash.
const PIECE_ENTRY_LEN: usize = 4 + 32;

/// A component of a snapshot: the rows of one column family that the snapshot
/// ships, in the current database format's encoding.
///
/// The discriminants are the wire ids, dense from zero in the order the
/// manifest lists the components.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u16)]
pub enum ComponentId {
    /// `hash_by_height`.
    HashByHeight = 0,
    /// `height_by_hash`.
    HeightByHash = 1,
    /// `block_header_by_height`.
    Headers = 2,
    /// `tx_by_loc`: the bodies of genesis and the snapshot block.
    Transactions = 3,
    /// `hash_by_tx_loc`.
    HashByTxLoc = 4,
    /// `tx_loc_by_hash`.
    TxLocByHash = 5,
    /// `block_info`.
    BlockInfo = 6,
    /// `utxo_by_out_loc`: the UTXO set.
    Utxos = 7,
    /// `balance_by_transparent_addr`.
    AddressBalances = 8,
    /// `utxo_loc_by_transparent_addr_loc`.
    AddressUtxos = 9,
    /// `tx_loc_by_transparent_addr_loc`.
    AddressTxs = 10,
    /// `sprout_nullifiers`.
    SproutNullifiers = 11,
    /// `sapling_nullifiers`.
    SaplingNullifiers = 12,
    /// `orchard_nullifiers`.
    OrchardNullifiers = 13,
    /// `ironwood_nullifiers`.
    IronwoodNullifiers = 14,
    /// `sprout_anchors`: each root with its whole Sprout tree.
    SproutAnchors = 15,
    /// `sapling_anchors`.
    SaplingAnchors = 16,
    /// `orchard_anchors`.
    OrchardAnchors = 17,
    /// `ironwood_anchors`.
    IronwoodAnchors = 18,
    /// `sapling_note_commitment_tree`.
    SaplingTree = 19,
    /// `orchard_note_commitment_tree`.
    OrchardTree = 20,
    /// `ironwood_note_commitment_tree`.
    IronwoodTree = 21,
    /// `sapling_note_commitment_subtree`.
    SaplingSubtrees = 22,
    /// `orchard_note_commitment_subtree`.
    OrchardSubtrees = 23,
    /// `ironwood_note_commitment_subtree`.
    IronwoodSubtrees = 24,
    /// `sprout_note_commitment_tree`: the Sprout tree at the snapshot height.
    SproutTipTree = 25,
    /// `history_tree`.
    HistoryTree = 26,
    /// `tip_chain_value_pool`.
    ValuePools = 27,
}

impl ComponentId {
    /// Every component, in manifest order: `ALL[id]` is the component with
    /// wire id `id`.
    pub const ALL: [ComponentId; 28] = [
        ComponentId::HashByHeight,
        ComponentId::HeightByHash,
        ComponentId::Headers,
        ComponentId::Transactions,
        ComponentId::HashByTxLoc,
        ComponentId::TxLocByHash,
        ComponentId::BlockInfo,
        ComponentId::Utxos,
        ComponentId::AddressBalances,
        ComponentId::AddressUtxos,
        ComponentId::AddressTxs,
        ComponentId::SproutNullifiers,
        ComponentId::SaplingNullifiers,
        ComponentId::OrchardNullifiers,
        ComponentId::IronwoodNullifiers,
        ComponentId::SproutAnchors,
        ComponentId::SaplingAnchors,
        ComponentId::OrchardAnchors,
        ComponentId::IronwoodAnchors,
        ComponentId::SaplingTree,
        ComponentId::OrchardTree,
        ComponentId::IronwoodTree,
        ComponentId::SaplingSubtrees,
        ComponentId::OrchardSubtrees,
        ComponentId::IronwoodSubtrees,
        ComponentId::SproutTipTree,
        ComponentId::HistoryTree,
        ComponentId::ValuePools,
    ];

    /// The number of components: every manifest lists each exactly once.
    pub const COUNT: u16 = Self::ALL.len() as u16;

    /// Returns the component with wire id `id`, or `None` for an id this
    /// format does not define.
    pub fn from_u16(id: u16) -> Option<Self> {
        Self::ALL.get(usize::from(id)).copied()
    }

    /// Returns the component's wire id.
    pub fn to_u16(self) -> u16 {
        // The enum is `repr(u16)`, so the cast is the discriminant.
        self as u16
    }

    /// Returns the name of the column family the component's rows come from
    /// and go to.
    pub fn column_family(self) -> &'static str {
        match self {
            ComponentId::HashByHeight => "hash_by_height",
            ComponentId::HeightByHash => "height_by_hash",
            ComponentId::Headers => "block_header_by_height",
            ComponentId::Transactions => "tx_by_loc",
            ComponentId::HashByTxLoc => "hash_by_tx_loc",
            ComponentId::TxLocByHash => "tx_loc_by_hash",
            ComponentId::BlockInfo => "block_info",
            ComponentId::Utxos => "utxo_by_out_loc",
            ComponentId::AddressBalances => "balance_by_transparent_addr",
            ComponentId::AddressUtxos => "utxo_loc_by_transparent_addr_loc",
            ComponentId::AddressTxs => "tx_loc_by_transparent_addr_loc",
            ComponentId::SproutNullifiers => "sprout_nullifiers",
            ComponentId::SaplingNullifiers => "sapling_nullifiers",
            ComponentId::OrchardNullifiers => "orchard_nullifiers",
            ComponentId::IronwoodNullifiers => "ironwood_nullifiers",
            ComponentId::SproutAnchors => "sprout_anchors",
            ComponentId::SaplingAnchors => "sapling_anchors",
            ComponentId::OrchardAnchors => "orchard_anchors",
            ComponentId::IronwoodAnchors => "ironwood_anchors",
            ComponentId::SaplingTree => "sapling_note_commitment_tree",
            ComponentId::OrchardTree => "orchard_note_commitment_tree",
            ComponentId::IronwoodTree => "ironwood_note_commitment_tree",
            ComponentId::SaplingSubtrees => "sapling_note_commitment_subtree",
            ComponentId::OrchardSubtrees => "orchard_note_commitment_subtree",
            ComponentId::IronwoodSubtrees => "ironwood_note_commitment_subtree",
            ComponentId::SproutTipTree => "sprout_note_commitment_tree",
            ComponentId::HistoryTree => "history_tree",
            ComponentId::ValuePools => "tip_chain_value_pool",
        }
    }

    /// Whether the component's values are canonicalized (design, "Snapshot
    /// contents"): decoded
    /// with `FromDisk` and re-encoded with the current `IntoDisk`, with cached
    /// roots recomputed, by the exporter and again by the importer. Every
    /// other component is copied raw, because it has a single encoding.
    pub fn is_canonicalized(self) -> bool {
        matches!(
            self,
            ComponentId::BlockInfo
                | ComponentId::ValuePools
                | ComponentId::HistoryTree
                | ComponentId::AddressBalances
                | ComponentId::SaplingTree
                | ComponentId::OrchardTree
                | ComponentId::IronwoodTree
                | ComponentId::SproutTipTree
                | ComponentId::SproutAnchors
        )
    }
}

impl fmt::Display for ComponentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.column_family())
    }
}

/// A snapshot manifest: what the snapshot is of, and the hash and size of
/// every piece.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    /// The hash of the network's genesis block.
    pub genesis_hash: block::Hash,

    /// The snapshot height `H`.
    pub height: block::Height,

    /// The hash of block `H`.
    pub block_hash: block::Hash,

    /// The first height with a complete block: `L = max(1, H - 2303)`.
    pub window_start: block::Height,

    /// The major database format version the rows are encoded in.
    pub db_format_major: u16,

    /// Which indexes the snapshot carries: [`INDEX_MODE_DEFAULT`].
    pub index_mode: u8,

    /// The hash of the consensus parameters, checkpoints excluded, on Regtest
    /// and custom testnets; `None` on Mainnet and the default Testnet, whose
    /// parameters the genesis hash identifies.
    pub parameters_hash: Option<[u8; 32]>,

    /// Every component, in [`ComponentId::ALL`] order.
    pub components: Vec<ComponentManifest>,
}

/// One component's entry in a manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentManifest {
    /// The component.
    pub id: ComponentId,

    /// The number of records across the component's pieces.
    pub entry_count: u64,

    /// The pieces, in key order.
    pub pieces: Vec<PieceManifest>,
}

/// One piece's entry in a manifest.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PieceManifest {
    /// The piece's size in bytes: at least one, at most [`MAX_PIECE_BYTES`].
    pub size: u32,

    /// The piece's hash.
    pub hash: ObjectHash,
}

impl Manifest {
    /// Whether a manifest for `network` carries a parameters hash: Mainnet and
    /// the default Testnet are identified by their genesis hash alone.
    pub fn needs_parameters_hash(network: &Network) -> bool {
        !(*network == Network::Mainnet || network.is_default_testnet())
    }

    /// Encodes the manifest, after checking that it is well-formed.
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        self.check()?;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&SNAPSHOT_FORMAT.to_le_bytes());
        bytes.extend_from_slice(&self.genesis_hash.0);
        bytes.extend_from_slice(&self.height.0.to_le_bytes());
        bytes.extend_from_slice(&self.block_hash.0);
        bytes.extend_from_slice(&self.window_start.0.to_le_bytes());
        bytes.extend_from_slice(&self.db_format_major.to_le_bytes());
        bytes.push(self.index_mode);
        if let Some(parameters_hash) = &self.parameters_hash {
            bytes.extend_from_slice(parameters_hash);
        }
        bytes.extend_from_slice(&ComponentId::COUNT.to_le_bytes());
        for component in &self.components {
            bytes.extend_from_slice(&component.id.to_u16().to_le_bytes());
            bytes.extend_from_slice(&component.entry_count.to_le_bytes());
            // `check` has already rejected a piece list longer than `u32::MAX`.
            let piece_count =
                u32::try_from(component.pieces.len()).map_err(|_| FormatError::TooManyPieces {
                    component: component.id,
                    count: component.pieces.len(),
                })?;
            bytes.extend_from_slice(&piece_count.to_le_bytes());
            for piece in &component.pieces {
                bytes.extend_from_slice(&piece.size.to_le_bytes());
                bytes.extend_from_slice(&piece.hash.0);
            }
        }

        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(FormatError::ManifestTooLarge { len: bytes.len() });
        }
        Ok(bytes)
    }

    /// Decodes a complete manifest for `network`, which decides whether a
    /// parameters hash is present.
    ///
    /// Every count is checked against the bytes that follow it before any
    /// allocation, and the manifest is checked as [`Self::encode`] checks it.
    pub fn decode(bytes: &[u8], network: &Network) -> Result<Self, FormatError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(FormatError::ManifestTooLarge { len: bytes.len() });
        }
        let mut reader = Reader::new(bytes);

        if reader.take(MAGIC.len(), "magic")? != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let snapshot_format = reader.u16("snapshot format")?;
        if snapshot_format != SNAPSHOT_FORMAT {
            return Err(FormatError::UnsupportedFormat(snapshot_format));
        }

        let genesis_hash = block::Hash(reader.hash("genesis hash")?);
        let height = block::Height(reader.u32("height")?);
        let block_hash = block::Hash(reader.hash("block hash")?);
        let window_start = block::Height(reader.u32("window start")?);
        let db_format_major = reader.u16("database format major version")?;
        let index_mode = reader.u8("index mode")?;
        let parameters_hash = if Self::needs_parameters_hash(network) {
            Some(reader.hash("parameters hash")?)
        } else {
            None
        };

        let component_count = reader.u16("component count")?;
        if component_count != ComponentId::COUNT {
            return Err(FormatError::WrongComponentCount(component_count));
        }
        let mut components = Vec::with_capacity(ComponentId::ALL.len());
        for _ in 0..component_count {
            let id = reader.u16("component id")?;
            let id = ComponentId::from_u16(id).ok_or(FormatError::UnknownComponent(id))?;
            let entry_count = reader.u64("entry count")?;
            let piece_count = reader.u32("piece count")?;

            // The piece list must be present in full before it is allocated, so a false count
            // costs nothing: at most `MAX_MANIFEST_BYTES / PIECE_ENTRY_LEN` entries are ever
            // allocated.
            let piece_count = usize::try_from(piece_count)
                .ok()
                .filter(|count| {
                    count
                        .checked_mul(PIECE_ENTRY_LEN)
                        .is_some_and(|needed| needed <= reader.remaining())
                })
                .ok_or(FormatError::Truncated { what: "piece list" })?;
            let mut pieces = Vec::with_capacity(piece_count);
            for _ in 0..piece_count {
                let size = reader.u32("piece size")?;
                let hash = ObjectHash(reader.hash("piece hash")?);
                pieces.push(PieceManifest { size, hash });
            }

            components.push(ComponentManifest {
                id,
                entry_count,
                pieces,
            });
        }
        reader.finish("manifest")?;

        let manifest = Manifest {
            genesis_hash,
            height,
            block_hash,
            window_start,
            db_format_major,
            index_mode,
            parameters_hash,
            components,
        };
        manifest.check()?;
        Ok(manifest)
    }

    /// Checks the rules of the format that hold for every manifest: the
    /// heights, the index mode, the component list, and each piece's size.
    pub fn check(&self) -> Result<(), FormatError> {
        for height in [self.height, self.window_start] {
            if height > block::Height::MAX {
                return Err(FormatError::InvalidHeight(height.0));
            }
        }
        if self.window_start > self.height {
            return Err(FormatError::WindowAboveHeight {
                window_start: self.window_start.0,
                height: self.height.0,
            });
        }
        if self.index_mode != INDEX_MODE_DEFAULT {
            return Err(FormatError::UnknownIndexMode(self.index_mode));
        }

        let component_count = u16::try_from(self.components.len())
            .map_err(|_| FormatError::WrongComponentCount(u16::MAX))?;
        if component_count != ComponentId::COUNT {
            return Err(FormatError::WrongComponentCount(component_count));
        }
        for (component, expected) in self.components.iter().zip(ComponentId::ALL) {
            if component.id != expected {
                return Err(FormatError::ComponentOutOfOrder {
                    expected,
                    found: component.id,
                });
            }
            let piece_count =
                u32::try_from(component.pieces.len()).map_err(|_| FormatError::TooManyPieces {
                    component: component.id,
                    count: component.pieces.len(),
                })?;
            if (component.entry_count == 0) != (piece_count == 0) {
                return Err(FormatError::InconsistentCounts {
                    component: component.id,
                    entry_count: component.entry_count,
                    piece_count,
                });
            }
            for piece in &component.pieces {
                if piece.size == 0 {
                    return Err(FormatError::EmptyPiece {
                        component: component.id,
                    });
                }
                // `MAX_PIECE_BYTES` is 16 MiB, which fits in a `u32`.
                if piece.size as usize > MAX_PIECE_BYTES {
                    return Err(FormatError::PieceTooLarge {
                        component: component.id,
                        size: piece.size,
                    });
                }
            }
        }
        Ok(())
    }

    /// Returns the entry of `component`.
    ///
    /// # Panics
    ///
    /// If the manifest does not pass [`Self::check`].
    pub fn component(&self, component: ComponentId) -> &ComponentManifest {
        let entry = &self.components[usize::from(component.to_u16())];
        assert_eq!(
            entry.id, component,
            "manifests list every component once, in id order"
        );
        entry
    }

    /// Returns every piece with its component and its index in the component.
    pub fn pieces(&self) -> impl Iterator<Item = (ComponentId, u32, PieceManifest)> + '_ {
        self.components.iter().flat_map(|component| {
            component
                .pieces
                .iter()
                .enumerate()
                .map(move |(index, piece)| {
                    // `check` bounds piece lists by `u32::MAX`, and `decode` by the manifest size.
                    (component.id, index as u32, *piece)
                })
        })
    }

    /// Returns the total size of every piece, in bytes.
    pub fn total_piece_bytes(&self) -> u64 {
        self.pieces()
            .map(|(_, _, piece)| u64::from(piece.size))
            .sum()
    }
}

/// A cursor over the bytes of a manifest that reports a truncated field
/// rather than reading past the end.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes }
    }

    /// The bytes not read yet.
    fn remaining(&self) -> usize {
        self.bytes.len()
    }

    /// Takes the next `len` bytes of `what`.
    fn take(&mut self, len: usize, what: &'static str) -> Result<&'a [u8], FormatError> {
        if self.bytes.len() < len {
            return Err(FormatError::Truncated { what });
        }
        let (taken, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self, what: &'static str) -> Result<[u8; N], FormatError> {
        // `take` returns exactly `N` bytes, so the conversion only fails if it did not.
        self.take(N, what)?
            .try_into()
            .map_err(|_| FormatError::Truncated { what })
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, FormatError> {
        Ok(self.array::<1>(what)?[0])
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.array(what)?))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.array(what)?))
    }

    fn u64(&mut self, what: &'static str) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.array(what)?))
    }

    fn hash(&mut self, what: &'static str) -> Result<[u8; 32], FormatError> {
        self.array(what)
    }

    /// Reports any bytes left as trailing bytes after `what`.
    fn finish(self, what: &'static str) -> Result<(), FormatError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(FormatError::TrailingBytes {
                what,
                count: self.bytes.len(),
            })
        }
    }
}

/// One record of a piece: a column family key and its value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PieceRecord {
    /// The key, in the current database format's encoding.
    pub key: Vec<u8>,

    /// The value, in the current database format's encoding.
    pub value: Vec<u8>,
}

impl PieceRecord {
    /// Returns the encoded length of a record with a `key_len` byte key and a
    /// `value_len` byte value, or `None` if it is over [`MAX_PIECE_BYTES`].
    pub fn encoded_len(key_len: usize, value_len: usize) -> Option<usize> {
        compact_size_len(key_len)
            .checked_add(key_len)?
            .checked_add(compact_size_len(value_len))?
            .checked_add(value_len)
            .filter(|len| *len <= MAX_PIECE_BYTES)
    }

    /// Writes the record `key` → `value` to `writer`, and returns its encoded
    /// length.
    pub fn write<W: Write>(key: &[u8], value: &[u8], mut writer: W) -> Result<usize, FormatError> {
        let len = Self::encoded_len(key.len(), value.len()).ok_or(FormatError::RecordTooLarge {
            // Both lengths are in memory, so they fit in a `u64`.
            len: key.len() as u64 + value.len() as u64,
        })?;
        write_compact_size(&mut writer, key.len())?;
        writer.write_all(key)?;
        write_compact_size(&mut writer, value.len())?;
        writer.write_all(value)?;
        Ok(len)
    }

    /// Encodes the record.
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        let mut bytes = Vec::new();
        Self::write(&self.key, &self.value, &mut bytes)?;
        Ok(bytes)
    }
}

/// The encoded length of `value` as a CompactSize.
fn compact_size_len(value: usize) -> usize {
    match value {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// Writes `value` as a canonical CompactSize.
fn write_compact_size<W: Write>(writer: W, value: usize) -> Result<(), FormatError> {
    // `usize` always fits in `u64` on supported targets.
    CompactSize64::from(value as u64).zcash_serialize(writer)?;
    Ok(())
}

/// Writes the records of one piece, in key order, within
/// [`MAX_PIECE_BYTES`].
pub struct PieceWriter<W: Write> {
    writer: W,
    /// The key of the last record, which the next key must be above.
    last_key: Option<Vec<u8>>,
    bytes_written: usize,
    records: u64,
}

impl<W: Write> PieceWriter<W> {
    /// Returns a writer of an empty piece to `writer`.
    pub fn new(writer: W) -> Self {
        PieceWriter {
            writer,
            last_key: None,
            bytes_written: 0,
            records: 0,
        }
    }

    /// Returns whether the record `key` → `value` fits in the piece.
    ///
    /// Returns an error if the record is over the piece limit on its own, so
    /// it fits in no piece.
    pub fn fits(&self, key: &[u8], value: &[u8]) -> Result<bool, FormatError> {
        let len = PieceRecord::encoded_len(key.len(), value.len()).ok_or(
            FormatError::RecordTooLarge {
                // Both lengths are in memory, so they fit in a `u64`.
                len: key.len() as u64 + value.len() as u64,
            },
        )?;
        Ok(self
            .bytes_written
            .checked_add(len)
            .is_some_and(|total| total <= MAX_PIECE_BYTES))
    }

    /// Appends the record `key` → `value`.
    ///
    /// Returns [`FormatError::PieceFull`] if the record does not fit, in
    /// which case the piece is unchanged and the record goes in the next
    /// piece, and [`FormatError::KeyNotIncreasing`] if `key` is not above the
    /// last key.
    pub fn push(&mut self, key: &[u8], value: &[u8]) -> Result<(), FormatError> {
        if self.last_key.as_deref().is_some_and(|last| key <= last) {
            return Err(FormatError::KeyNotIncreasing);
        }
        if !self.fits(key, value)? {
            return Err(FormatError::PieceFull);
        }
        let len = PieceRecord::write(key, value, &mut self.writer)?;
        self.bytes_written += len;
        self.records += 1;
        self.last_key = Some(key.to_vec());
        Ok(())
    }

    /// The encoded length of the records written so far.
    pub fn bytes_written(&self) -> usize {
        self.bytes_written
    }

    /// The number of records written so far.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Whether no record has been written.
    pub fn is_empty(&self) -> bool {
        self.records == 0
    }

    /// Flushes the piece and returns the underlying writer.
    pub fn finish(mut self) -> Result<W, FormatError> {
        self.writer.flush()?;
        Ok(self.writer)
    }
}

/// Reads the records of one piece from a byte stream, checking the format
/// as it goes: canonical lengths, strictly increasing keys, and the piece
/// limit.
///
/// Each record is decoded once its bytes have arrived, and nothing is
/// allocated from a length before it is checked against the piece limit.
pub struct PieceReader<R: Read> {
    reader: R,
    /// The key of the last record, which the next key must be above.
    last_key: Option<Vec<u8>>,
    bytes_read: usize,
    records: u64,
}

impl<R: Read> PieceReader<R> {
    /// Returns a reader of the piece in `reader`.
    pub fn new(reader: R) -> Self {
        PieceReader {
            reader,
            last_key: None,
            bytes_read: 0,
            records: 0,
        }
    }

    /// Returns the next record, or `None` once the stream ends at a record
    /// boundary.
    pub fn next_record(&mut self) -> Result<Option<PieceRecord>, FormatError> {
        let Some(key_len) = self.compact_size_or_end("record key length")? else {
            return Ok(None);
        };
        let key = self.bytes(key_len, "record key")?;
        let value_len =
            self.compact_size_or_end("record value length")?
                .ok_or(FormatError::Truncated {
                    what: "record value length",
                })?;
        let value = self.bytes(value_len, "record value")?;

        if self
            .last_key
            .as_deref()
            .is_some_and(|last| key.as_slice() <= last)
        {
            return Err(FormatError::KeyNotIncreasing);
        }
        self.last_key = Some(key.clone());
        self.records += 1;
        Ok(Some(PieceRecord { key, value }))
    }

    /// The number of bytes read so far.
    pub fn bytes_read(&self) -> usize {
        self.bytes_read
    }

    /// The number of records read so far.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Counts `len` more bytes against the piece limit.
    fn account(&mut self, len: usize) -> Result<(), FormatError> {
        self.bytes_read = self
            .bytes_read
            .checked_add(len)
            .filter(|total| *total <= MAX_PIECE_BYTES)
            .ok_or(FormatError::PieceBytesExceeded)?;
        Ok(())
    }

    /// Reads a canonical CompactSize, or `None` if the stream ends before it.
    fn compact_size_or_end(&mut self, what: &'static str) -> Result<Option<u64>, FormatError> {
        let mut bytes = [0u8; 9];
        // The first byte is read on its own, to tell the end of the stream from a record. An
        // interrupted read is retried, as `read_exact` retries it for the other bytes.
        let read = loop {
            match self.reader.read(&mut bytes[..1]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if read == 0 {
            return Ok(None);
        }
        let encoded_len = match bytes[0] {
            0x00..=0xFC => 1,
            0xFD => 3,
            0xFE => 5,
            0xFF => 9,
        };
        self.account(encoded_len)?;
        self.reader
            .read_exact(&mut bytes[1..encoded_len])
            .map_err(|error| truncated(error, what))?;
        CompactSize64::zcash_deserialize(&bytes[..encoded_len])
            .map(|size| Some(u64::from(size)))
            .map_err(|source| FormatError::InvalidCompactSize { what, source })
    }

    /// Reads exactly `len` bytes of `what`, after checking `len` against the
    /// piece limit.
    fn bytes(&mut self, len: u64, what: &'static str) -> Result<Vec<u8>, FormatError> {
        let len = usize::try_from(len)
            .ok()
            .filter(|len| *len <= MAX_PIECE_BYTES)
            .ok_or(FormatError::RecordTooLarge { len })?;
        self.account(len)?;

        // Grown as the bytes arrive, never reserved from the length.
        let mut bytes = Vec::new();
        // `len` is at most `MAX_PIECE_BYTES`, so it fits in a `u64`.
        (&mut self.reader)
            .take(len as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() != len {
            return Err(FormatError::Truncated { what });
        }
        Ok(bytes)
    }
}

/// Maps the end of the stream in the middle of `what` to a truncation, and
/// keeps any other I/O error.
fn truncated(error: io::Error, what: &'static str) -> FormatError {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        FormatError::Truncated { what }
    } else {
        FormatError::Io(error)
    }
}

/// Why a manifest or piece does not follow the format.
#[derive(Debug, Error)]
pub enum FormatError {
    /// The manifest is over [`MAX_MANIFEST_BYTES`].
    #[error("manifest is {len} bytes, over the limit of {limit} bytes", limit = MAX_MANIFEST_BYTES)]
    ManifestTooLarge {
        /// The manifest's length.
        len: usize,
    },

    /// The bytes end in the middle of a field.
    #[error("{what} is truncated")]
    Truncated {
        /// The field that is incomplete.
        what: &'static str,
    },

    /// Bytes follow a complete manifest.
    #[error("{count} trailing bytes after the {what}")]
    TrailingBytes {
        /// What the bytes follow.
        what: &'static str,
        /// The number of trailing bytes.
        count: usize,
    },

    /// The manifest does not start with [`MAGIC`].
    #[error("manifest does not start with the snapshot magic string")]
    BadMagic,

    /// The manifest's snapshot format is not [`SNAPSHOT_FORMAT`].
    #[error("unsupported snapshot format {0}")]
    UnsupportedFormat(u16),

    /// The manifest's index mode is not [`INDEX_MODE_DEFAULT`].
    #[error("unknown index mode {0}")]
    UnknownIndexMode(u8),

    /// A height is above the maximum block height.
    #[error("height {0} is above the maximum block height")]
    InvalidHeight(u32),

    /// The window starts above the snapshot height.
    #[error("window start {window_start} is above the snapshot height {height}")]
    WindowAboveHeight {
        /// The window start.
        window_start: u32,
        /// The snapshot height.
        height: u32,
    },

    /// The manifest does not list exactly [`ComponentId::COUNT`] components.
    #[error("manifest lists {0} components, not {count}", count = ComponentId::COUNT)]
    WrongComponentCount(u16),

    /// A component id this format does not define.
    #[error("unknown component id {0}")]
    UnknownComponent(u16),

    /// The components are not in [`ComponentId::ALL`] order, or one appears
    /// twice.
    #[error("component {found} is out of order: expected {expected}")]
    ComponentOutOfOrder {
        /// The component expected at this position.
        expected: ComponentId,
        /// The component found there.
        found: ComponentId,
    },

    /// A component has an empty piece.
    #[error("component {component} has an empty piece")]
    EmptyPiece {
        /// The component.
        component: ComponentId,
    },

    /// A piece is over [`MAX_PIECE_BYTES`].
    #[error(
        "component {component} has a {size} byte piece, over the limit of {limit} bytes",
        limit = MAX_PIECE_BYTES
    )]
    PieceTooLarge {
        /// The component.
        component: ComponentId,
        /// The piece's size.
        size: u32,
    },

    /// A component has entries without pieces, or pieces without entries.
    #[error("component {component} lists {entry_count} entries in {piece_count} pieces")]
    InconsistentCounts {
        /// The component.
        component: ComponentId,
        /// The entry count.
        entry_count: u64,
        /// The piece count.
        piece_count: u32,
    },

    /// A component has more pieces than the format can count.
    #[error("component {component} has {count} pieces, more than the format can list")]
    TooManyPieces {
        /// The component.
        component: ComponentId,
        /// The number of pieces.
        count: usize,
    },

    /// A record length is not a canonical CompactSize.
    #[error("invalid CompactSize for the {what}: {source}")]
    InvalidCompactSize {
        /// The length field.
        what: &'static str,
        /// The underlying error.
        #[source]
        source: SerializationError,
    },

    /// A record's key is not above the previous record's key.
    #[error("record key is not above the previous record's key")]
    KeyNotIncreasing,

    /// A record is over [`MAX_PIECE_BYTES`] on its own.
    #[error("record of {len} bytes is over the piece limit of {limit} bytes", limit = MAX_PIECE_BYTES)]
    RecordTooLarge {
        /// The record's length, or a length field's claim.
        len: u64,
    },

    /// A piece is over [`MAX_PIECE_BYTES`].
    #[error("piece is over the limit of {limit} bytes", limit = MAX_PIECE_BYTES)]
    PieceBytesExceeded,

    /// The record does not fit in the piece being written.
    #[error("the piece is full")]
    PieceFull,

    /// A value of a canonicalized component does not decode.
    #[error("invalid {component} value: {reason}")]
    InvalidValue {
        /// The component.
        component: ComponentId,
        /// Why the value does not decode.
        reason: &'static str,
    },

    /// A note commitment tree's cached root is not its root.
    #[error("a {component} tree's cached root is not its root")]
    WrongCachedRoot {
        /// The component.
        component: ComponentId,
    },

    /// Reading or writing the bytes failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}
