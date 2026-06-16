//! Known-hash chunk format **v2**: the deterministic byte layout of a chunk.
//!
//! A v2 chunk is the on-the-wire and on-disk representation of a span of up to
//! [`HASHES_PER_CHUNK`](super::HASHES_PER_CHUNK) consecutive blocks. It carries,
//! for the heights in the span:
//!
//! - one block hash per block (Zebra's internal serialized byte order),
//! - optionally one approximate size hint per block (the stored serialized size
//!   quantized into [`SIZE_HINT_UNIT`](super::SIZE_HINT_UNIT) units), and
//! - optionally the sapling/orchard note-commitment-tree roots at the heights in
//!   the span that *update* each tree, stored sparsely in ascending order.
//!
//! # Determinism
//!
//! The byte layout is a deterministic function of the chain: any synced node
//! regenerates a byte-identical chunk `N`, so `SHA-256(chunk N)` matches the
//! pinned `chunk_hashes[N]` constant in [`KnownHashListSpec`](super::KnownHashListSpec).
//! This is the trust root for P2P distribution (see
//! `docs/design/p2p-snapshot-distribution.md`): peers regenerate chunks from
//! their own finalized state, and downloaders verify them content-addressed
//! against the pinned hashes.
//!
//! # Byte layout
//!
//! For a span of `n` blocks (`n <= HASHES_PER_CHUNK`):
//!
//! ```text
//! HEADER (16 bytes):
//!   magic        b"ZKH2"   (4 bytes)
//!   version      u8 = 2     (1 byte)
//!   flags        u8         (1 byte; bit0 = has_hints, bit1 = has_tree_roots)
//!   reserved     u16 = 0    (2 bytes, little-endian)
//!   block_count  u32 = n    (4 bytes, little-endian)
//! block hashes:  n × 32 bytes (internal byte order)
//! size hints (iff flags.has_hints):     n × 1 byte
//! sapling roots (iff flags.has_tree_roots):
//!   count u32 LE, then count × (rel_height u32 LE in 0..n, root [u8; 32])
//!   ascending rel_height, one record per updating height
//! orchard roots (iff flags.has_tree_roots):
//!   count u32 LE, then count × (rel_height u32 LE in 0..n, root [u8; 32])
//! ```

use thiserror::Error;

use super::{HASHES_PER_CHUNK, HASH_BYTES};

#[cfg(test)]
mod tests;

/// The 4-byte magic prefixing every v2 chunk: `ZKH2` ("Zebra Known Hashes v2").
///
/// Distinguishes v2 chunks from the legacy v1 layout (`n × 32` or `n × 33`
/// bytes, no header), which never begins with these bytes.
pub const MAGIC: [u8; 4] = *b"ZKH2";

/// The format version byte stored in the header, immediately after [`MAGIC`].
pub const VERSION: u8 = 2;

/// The fixed length of the v2 chunk header in bytes: magic (4) + version (1) +
/// flags (1) + reserved (2) + block_count (4).
pub const HEADER_LEN: usize = 16;

/// The trailing reserved bytes (offsets 12..16) that pad the named header fields
/// (12 bytes) out to the fixed [`HEADER_LEN`]. Always zero; validated on parse so
/// the spare space stays available for future header fields.
const RESERVED_TAIL: [u8; 4] = [0; 4];

/// The number of bytes each tree-root record occupies: a `u32` little-endian
/// relative height (4) followed by a 32-byte root.
const TREE_RECORD_LEN: usize = 4 + HASH_BYTES;

/// The flag bit (in the header `flags` byte) set when per-block size hints are
/// present, i.e. when the `n × 1` size-hint section follows the block hashes.
const FLAG_HAS_HINTS: u8 = 0b0000_0001;

/// The flag bit (in the header `flags` byte) set when the sapling and orchard
/// tree-root sections are present after the size hints.
const FLAG_HAS_TREE_ROOTS: u8 = 0b0000_0010;

/// The default size hint returned for a block whose chunk carries no hints, or
/// before a hinted section is present: the maximum (255 units), an always-safe
/// upper bound on any block's serialized size.
///
/// Mirrors the size-hint contract in [`super`]: a hint `w` means the block's
/// serialized size is at most `w × SIZE_HINT_UNIT` bytes, so 255 is the loosest
/// (largest) bound and never under-counts.
pub const DEFAULT_SIZE_HINT: u8 = 255;

/// A single tree-root update record: the within-span relative height that
/// updated the tree, and the tree's root as of that height.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TreeRoot {
    /// The within-span offset (`0..block_count`) of the updating height.
    pub rel_height: u32,

    /// The note-commitment-tree root as of `rel_height`, as
    /// `<[u8; 32]>::from(tree.root())`.
    pub root: [u8; HASH_BYTES],
}

/// Encodes a v2 chunk from explicit inputs.
///
/// `blocks` holds one `(block_hash, size_hint)` pair per block in the span, in
/// ascending height order. `with_hints` selects whether the size-hint section is
/// emitted (clear it to ship hash-only chunks); the per-pair hints are ignored
/// when it is `false`.
///
/// `sapling_roots` and `orchard_roots` are the sparse tree-root updates, each in
/// ascending `rel_height` order with every `rel_height < blocks.len()`. When
/// `with_tree_roots` is `true` both sections are written (either may be empty);
/// when it is `false` neither section is written and the lists are ignored.
///
/// Used by the snapshot emitter (release-time constants updater) and by
/// on-demand P2P serving, which must produce byte-identical output for the same
/// inputs so the chunk SHA-256 is stable.
///
/// # Panics
///
/// Panics if `blocks` is longer than [`HASHES_PER_CHUNK`], if a tree record's
/// `rel_height` is out of range, or if a tree-root list is not strictly
/// ascending — all of which are emitter bugs, never attacker-controlled input
/// (the parser validates untrusted bytes instead).
pub fn encode(
    blocks: &[([u8; HASH_BYTES], u8)],
    with_hints: bool,
    sapling_roots: &[TreeRoot],
    orchard_roots: &[TreeRoot],
    with_tree_roots: bool,
) -> Vec<u8> {
    let n = blocks.len();
    assert!(
        n <= HASHES_PER_CHUNK as usize,
        "v2 chunk span {n} exceeds HASHES_PER_CHUNK {HASHES_PER_CHUNK}",
    );
    // The block count is bounded by HASHES_PER_CHUNK (150,000), so it fits a u32.
    let block_count = n as u32;

    let mut flags = 0u8;
    if with_hints {
        flags |= FLAG_HAS_HINTS;
    }
    if with_tree_roots {
        flags |= FLAG_HAS_TREE_ROOTS;
    }

    // Pre-size the buffer so a full Mainnet chunk encodes without reallocating.
    let hints_len = if with_hints { n } else { 0 };
    let tree_len = if with_tree_roots {
        2 * 4 + (sapling_roots.len() + orchard_roots.len()) * TREE_RECORD_LEN
    } else {
        0
    };
    let mut out = Vec::with_capacity(HEADER_LEN + n * HASH_BYTES + hints_len + tree_len);

    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(flags);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&block_count.to_le_bytes());
    // Trailing reserved padding to the fixed 16-byte HEADER_LEN, zero by spec so
    // future header fields can grow into it without shifting any section offset.
    out.extend_from_slice(&RESERVED_TAIL);
    debug_assert_eq!(out.len(), HEADER_LEN, "header is exactly HEADER_LEN bytes");

    for (hash, _) in blocks {
        out.extend_from_slice(hash);
    }

    if with_hints {
        for (_, hint) in blocks {
            out.push(*hint);
        }
    }

    if with_tree_roots {
        encode_tree_section(&mut out, block_count, sapling_roots);
        encode_tree_section(&mut out, block_count, orchard_roots);
    }

    out
}

/// Appends one tree-root section (`count`, then the records) to `out`,
/// validating that records are in range and strictly ascending.
///
/// # Panics
///
/// Panics on an out-of-range or non-ascending `rel_height`; see [`encode`].
fn encode_tree_section(out: &mut Vec<u8>, block_count: u32, roots: &[TreeRoot]) {
    // The record count is bounded by the span length (<= HASHES_PER_CHUNK), so
    // it fits a u32.
    out.extend_from_slice(&(roots.len() as u32).to_le_bytes());

    let mut prev: Option<u32> = None;
    for record in roots {
        assert!(
            record.rel_height < block_count,
            "tree rel_height {} is outside the span 0..{block_count}",
            record.rel_height,
        );
        if let Some(prev) = prev {
            assert!(
                record.rel_height > prev,
                "tree rel_heights must be strictly ascending: {prev} then {}",
                record.rel_height,
            );
        }
        prev = Some(record.rel_height);

        out.extend_from_slice(&record.rel_height.to_le_bytes());
        out.extend_from_slice(&record.root);
    }
}

/// Errors validating or parsing a v2 chunk's bytes.
///
/// Returned for untrusted input (peer-supplied chunk bytes); each variant names
/// the precise structural failure so a caller can log it and drop the chunk
/// without dropping the peer.
#[derive(Error, Debug, PartialEq, Eq)]
pub enum ChunkV2Error {
    /// The buffer is shorter than the fixed 16-byte header.
    #[error("v2 chunk is {len} bytes, shorter than the {HEADER_LEN}-byte header")]
    ShortHeader {
        /// The actual buffer length.
        len: usize,
    },

    /// The leading 4 bytes are not the [`MAGIC`] `ZKH2`.
    #[error("v2 chunk magic mismatch: expected {expected:02x?}, got {actual:02x?}")]
    BadMagic {
        /// The expected magic bytes.
        expected: [u8; 4],
        /// The leading 4 bytes that were found.
        actual: [u8; 4],
    },

    /// The version byte is not [`VERSION`].
    #[error("v2 chunk version mismatch: expected {VERSION}, got {actual}")]
    BadVersion {
        /// The version byte that was found.
        actual: u8,
    },

    /// The reserved `u16` is non-zero (reserved-must-be-zero).
    #[error("v2 chunk reserved field must be zero, got {actual}")]
    NonZeroReserved {
        /// The reserved field value that was found.
        actual: u16,
    },

    /// An unknown flag bit (outside `has_hints` / `has_tree_roots`) is set.
    #[error("v2 chunk flags contain unknown bits: {flags:#010b}")]
    UnknownFlags {
        /// The full flags byte that was found.
        flags: u8,
    },

    /// The declared `block_count` exceeds [`HASHES_PER_CHUNK`].
    #[error("v2 chunk block_count {block_count} exceeds HASHES_PER_CHUNK {HASHES_PER_CHUNK}")]
    BlockCountTooLarge {
        /// The declared block count.
        block_count: u32,
    },

    /// The buffer is too short for the sections its header declares, or has
    /// trailing bytes after the last declared section.
    #[error(
        "v2 chunk body length mismatch: header declares {expected} bytes of \
         sections, buffer has {actual}"
    )]
    BodyLength {
        /// The total length the header + declared sections require.
        expected: usize,
        /// The actual buffer length.
        actual: usize,
    },

    /// A tree-root section's record `rel_height` is outside `0..block_count` or
    /// not strictly ascending.
    #[error(
        "v2 chunk tree record {index} has rel_height {rel_height}, which is \
         out of range or not strictly ascending (block_count {block_count})"
    )]
    BadTreeRecord {
        /// The 0-based index of the offending record within its section.
        index: usize,
        /// The offending relative height.
        rel_height: u32,
        /// The declared block count.
        block_count: u32,
    },
}

/// A parsed, validated view over a v2 chunk's bytes.
///
/// Borrows the underlying buffer (zero-copy): accessors slice into it rather
/// than copying. Construct with [`ParsedChunk::parse`], which validates the
/// header and the structural layout up front, so every accessor below is
/// infallible for in-range arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedChunk<'a> {
    /// The number of blocks in the span.
    block_count: u32,

    /// The `block_count × 32` block-hash bytes.
    hashes: &'a [u8],

    /// The `block_count × 1` size-hint bytes, or `None` if the chunk is
    /// hash-only (`has_hints` clear).
    hints: Option<&'a [u8]>,

    /// The sapling tree-root records (`rel_height`, root) pairs, packed
    /// `TREE_RECORD_LEN` bytes each, ascending by `rel_height`; empty when
    /// `has_tree_roots` is clear or the section has zero records.
    sapling: &'a [u8],

    /// The orchard tree-root records, like [`Self::sapling`].
    orchard: &'a [u8],
}

impl<'a> ParsedChunk<'a> {
    /// Validates `bytes` as a v2 chunk and returns a borrowing view.
    ///
    /// Checks the magic, version, reserved field, flag bits, block count, the
    /// exact total length implied by the flags, and that every tree record is
    /// in range and strictly ascending — so the chunk is fully trusted
    /// structurally once this returns `Ok`.
    // The `expect`s below convert fixed-width slices (already length-checked
    // above) into arrays, an infallible operation, not a hidden error path.
    #[allow(clippy::unwrap_in_result)]
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ChunkV2Error> {
        if bytes.len() < HEADER_LEN {
            return Err(ChunkV2Error::ShortHeader { len: bytes.len() });
        }

        let magic: [u8; 4] = bytes[0..4]
            .try_into()
            .expect("slice is exactly 4 bytes long");
        if magic != MAGIC {
            return Err(ChunkV2Error::BadMagic {
                expected: MAGIC,
                actual: magic,
            });
        }

        let version = bytes[4];
        if version != VERSION {
            return Err(ChunkV2Error::BadVersion { actual: version });
        }

        let flags = bytes[5];
        if flags & !(FLAG_HAS_HINTS | FLAG_HAS_TREE_ROOTS) != 0 {
            return Err(ChunkV2Error::UnknownFlags { flags });
        }
        let has_hints = flags & FLAG_HAS_HINTS != 0;
        let has_tree_roots = flags & FLAG_HAS_TREE_ROOTS != 0;

        let reserved = u16::from_le_bytes(
            bytes[6..8]
                .try_into()
                .expect("slice is exactly 2 bytes long"),
        );
        if reserved != 0 {
            return Err(ChunkV2Error::NonZeroReserved { actual: reserved });
        }

        let block_count = u32::from_le_bytes(
            bytes[8..12]
                .try_into()
                .expect("slice is exactly 4 bytes long"),
        );
        if block_count > HASHES_PER_CHUNK {
            return Err(ChunkV2Error::BlockCountTooLarge { block_count });
        }

        // The trailing reserved padding (offsets 12..16) must be zero, like the
        // u16 reserved field; reuse NonZeroReserved with its low half packed for
        // a clear, single error variant.
        if bytes[12..HEADER_LEN] != RESERVED_TAIL {
            let actual = u16::from_le_bytes(
                bytes[12..14]
                    .try_into()
                    .expect("slice is exactly 2 bytes long"),
            );
            return Err(ChunkV2Error::NonZeroReserved { actual });
        }
        // block_count <= HASHES_PER_CHUNK (150,000), so these byte counts fit a
        // usize on every supported platform (>= 32-bit) without overflow.
        let n = block_count as usize;

        let mut offset = HEADER_LEN;

        let hashes_len = n * HASH_BYTES;
        let hashes = slice_section(bytes, &mut offset, hashes_len)?;

        let hints = if has_hints {
            Some(slice_section(bytes, &mut offset, n)?)
        } else {
            None
        };

        let (sapling, orchard) = if has_tree_roots {
            let sapling = parse_tree_section(bytes, &mut offset, block_count)?;
            let orchard = parse_tree_section(bytes, &mut offset, block_count)?;
            (sapling, orchard)
        } else {
            (&bytes[offset..offset], &bytes[offset..offset])
        };

        if offset != bytes.len() {
            return Err(ChunkV2Error::BodyLength {
                expected: offset,
                actual: bytes.len(),
            });
        }

        Ok(Self {
            block_count,
            hashes,
            hints,
            sapling,
            orchard,
        })
    }

    /// The number of blocks in the span.
    pub fn block_count(&self) -> u32 {
        self.block_count
    }

    /// Whether the chunk carries per-block size hints.
    pub fn has_hints(&self) -> bool {
        self.hints.is_some()
    }

    /// The block hash at within-span offset `rel`.
    ///
    /// # Panics
    ///
    /// Panics if `rel >= block_count`; callers must bound `rel` to the span (the
    /// header's `block_count` is validated by [`parse`](Self::parse)).
    pub fn block_hash(&self, rel: u32) -> [u8; HASH_BYTES] {
        assert!(
            rel < self.block_count,
            "block_hash rel {rel} is outside the span 0..{}",
            self.block_count,
        );
        // rel < block_count, so this offset is within the validated hash section.
        let start = rel as usize * HASH_BYTES;
        self.hashes[start..start + HASH_BYTES]
            .try_into()
            .expect("hash section length is a verified multiple of HASH_BYTES")
    }

    /// The size hint at within-span offset `rel`, or [`DEFAULT_SIZE_HINT`] when
    /// the chunk is hash-only.
    ///
    /// # Panics
    ///
    /// Panics if `rel >= block_count`; see [`block_hash`](Self::block_hash).
    pub fn size_hint(&self, rel: u32) -> u8 {
        assert!(
            rel < self.block_count,
            "size_hint rel {rel} is outside the span 0..{}",
            self.block_count,
        );
        match self.hints {
            // rel < block_count == hints.len(), so this index is in bounds.
            Some(hints) => hints[rel as usize],
            None => DEFAULT_SIZE_HINT,
        }
    }

    /// The sapling tree root recorded at the largest `rel_height <= rel`, or
    /// `None` if no recorded height is at or before `rel`.
    ///
    /// Binary-searches the sparse, ascending sapling section. A node looking up
    /// the tree as of height `H` passes `rel = H - span_base`.
    pub fn sapling_root_at_or_before(&self, rel: u32) -> Option<[u8; HASH_BYTES]> {
        root_at_or_before(self.sapling, rel)
    }

    /// The orchard tree root recorded at the largest `rel_height <= rel`, or
    /// `None`; see [`sapling_root_at_or_before`](Self::sapling_root_at_or_before).
    pub fn orchard_root_at_or_before(&self, rel: u32) -> Option<[u8; HASH_BYTES]> {
        root_at_or_before(self.orchard, rel)
    }

    /// All sapling tree-root records, decoded in ascending `rel_height` order.
    ///
    /// Convenience for callers that write every recorded tree by height; the
    /// hot lookup path should use
    /// [`sapling_root_at_or_before`](Self::sapling_root_at_or_before) instead.
    pub fn sapling_roots(&self) -> Vec<TreeRoot> {
        decode_tree_records(self.sapling)
    }

    /// All orchard tree-root records; see [`sapling_roots`](Self::sapling_roots).
    pub fn orchard_roots(&self) -> Vec<TreeRoot> {
        decode_tree_records(self.orchard)
    }
}

/// Advances `offset` by `len` bytes, returning the slice, or
/// [`ChunkV2Error::BodyLength`] if the buffer is too short.
fn slice_section<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    len: usize,
) -> Result<&'a [u8], ChunkV2Error> {
    let end = offset.checked_add(len).ok_or(ChunkV2Error::BodyLength {
        // An overflowing section length cannot be satisfied by any buffer.
        expected: usize::MAX,
        actual: bytes.len(),
    })?;
    if end > bytes.len() {
        return Err(ChunkV2Error::BodyLength {
            expected: end,
            actual: bytes.len(),
        });
    }
    let section = &bytes[*offset..end];
    *offset = end;
    Ok(section)
}

/// Parses one tree-root section starting at `offset`: a `u32` count followed by
/// `count` records, validating each record is in range and strictly ascending.
///
/// Returns the packed records slice (without the count prefix) and advances
/// `offset` past the whole section.
// The `expect`s convert fixed-width slices into arrays, an infallible
// operation, not a hidden error path.
#[allow(clippy::unwrap_in_result)]
fn parse_tree_section<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    block_count: u32,
) -> Result<&'a [u8], ChunkV2Error> {
    let count_bytes = slice_section(bytes, offset, 4)?;
    let count = u32::from_le_bytes(
        count_bytes
            .try_into()
            .expect("slice is exactly 4 bytes long"),
    );

    // A section cannot hold more records than there are heights in the span; this
    // also bounds the byte length below well under usize::MAX.
    if count > block_count {
        return Err(ChunkV2Error::BadTreeRecord {
            index: count as usize,
            rel_height: count,
            block_count,
        });
    }

    // count <= block_count <= HASHES_PER_CHUNK, so the record bytes fit a usize.
    let records_len = count as usize * TREE_RECORD_LEN;
    let records = slice_section(bytes, offset, records_len)?;

    let mut prev: Option<u32> = None;
    for (index, record) in records.chunks_exact(TREE_RECORD_LEN).enumerate() {
        let rel_height = u32::from_le_bytes(
            record[0..4]
                .try_into()
                .expect("record starts with a 4-byte rel_height"),
        );
        let in_range = rel_height < block_count;
        let ascending = prev.is_none_or(|prev| rel_height > prev);
        if !in_range || !ascending {
            return Err(ChunkV2Error::BadTreeRecord {
                index,
                rel_height,
                block_count,
            });
        }
        prev = Some(rel_height);
    }

    Ok(records)
}

/// Binary-searches a packed tree-record section for the root recorded at the
/// largest `rel_height <= rel`, or `None`.
///
/// `records` is the validated, ascending packed slice (no count prefix); each
/// record is [`TREE_RECORD_LEN`] bytes (`u32` rel_height then 32-byte root).
fn root_at_or_before(records: &[u8], rel: u32) -> Option<[u8; HASH_BYTES]> {
    let n = records.len() / TREE_RECORD_LEN;
    if n == 0 {
        return None;
    }

    // Standard largest-key-<=-target binary search over the ascending records.
    let mut lo = 0usize;
    let mut hi = n;
    let mut found: Option<usize> = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mid_height = record_rel_height(records, mid);
        if mid_height <= rel {
            found = Some(mid);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    found.map(|i| record_root(records, i))
}

/// The `rel_height` of record `i` in a packed tree-record section.
fn record_rel_height(records: &[u8], i: usize) -> u32 {
    let start = i * TREE_RECORD_LEN;
    u32::from_le_bytes(
        records[start..start + 4]
            .try_into()
            .expect("record starts with a 4-byte rel_height"),
    )
}

/// The 32-byte root of record `i` in a packed tree-record section.
fn record_root(records: &[u8], i: usize) -> [u8; HASH_BYTES] {
    let start = i * TREE_RECORD_LEN + 4;
    records[start..start + HASH_BYTES]
        .try_into()
        .expect("record root is exactly HASH_BYTES long")
}

/// Decodes a packed tree-record section into owned [`TreeRoot`]s.
fn decode_tree_records(records: &[u8]) -> Vec<TreeRoot> {
    records
        .chunks_exact(TREE_RECORD_LEN)
        .map(|record| TreeRoot {
            rel_height: u32::from_le_bytes(
                record[0..4]
                    .try_into()
                    .expect("record starts with a 4-byte rel_height"),
            ),
            root: record[4..4 + HASH_BYTES]
                .try_into()
                .expect("record root is exactly HASH_BYTES long"),
        })
        .collect()
}

/// Whether `bytes` begins with the v2 [`MAGIC`], distinguishing a v2 chunk from
/// the legacy v1 layout.
///
/// Lets the loader pick the v1 or v2 path without a full parse; a v1 chunk is a
/// bare sequence of 32-byte hashes and never starts with `ZKH2`.
pub fn is_v2(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0..4] == MAGIC
}
