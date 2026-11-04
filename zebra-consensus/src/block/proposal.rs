//! The cache of validated block proposals, and decoding of mined submissions against it.
//!
//! A successful proposal check keeps the proposal's decoded block, its contextual verification
//! result, and its wire bytes. A submission whose bytes match a cached proposal, apart from the
//! nonce and Equihash solution, reuses them instead of being decoded and verified again.

use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{Arc, Weak},
};

use tokio::sync::watch;
use tokio_util::task::AbortOnDropHandle;

use zebra_chain::{
    block,
    serialization::{sha256d, SerializationError, ZcashDeserialize, ZcashSerialize},
    work::equihash,
};
use zebra_state as zs;

pub(super) type ProposalCache = VecDeque<PreparedProposal>;

#[derive(Debug)]
pub(super) struct PreparedProposal {
    pub(super) key: [u8; 32],
    pub(super) transaction_bytes: usize,
    pub(super) block: Arc<block::Block>,
    pub(super) contextual: Arc<zs::ContextuallyVerifiedBlock>,
    /// The proposal's wire bytes, so a submission is compared without serializing again.
    pub(super) wire: Arc<[u8]>,
    pub(super) retained_bytes: usize,
}

/// Bound retained proposal data, including unusually large spent-output scripts.
///
/// The shared conservative charge, not measured heap usage, is 66,198,006 bytes for a MAX_BLOCK_BYTES
/// proposal with 19,215 transactions using 104-byte one-input/one-P2PKH-output payments. Eight such
/// ordinary transparent proposals need about 505 MiB; 8 * 72 MiB leaves about 14% layout headroom.
/// Smaller consensus-valid transactions can pack more decoded structs into a block and retain fewer
/// entries. Eviction only loses cache hits, never validation, as for large spent-output scripts.
pub(super) const MAX_PROPOSAL_BYTES: usize = 8 * 72 * 1024 * 1024;

/// Charge once on a cancellable CPU worker, then publish only while the caller is still waiting.
pub(super) async fn cache_proposal(
    proposals: &watch::Sender<ProposalCache>,
    key: [u8; 32],
    block: Arc<block::Block>,
    transaction_bytes: usize,
    contextual: Arc<zs::ContextuallyVerifiedBlock>,
) {
    let (contextual, wire, retained_bytes) = AbortOnDropHandle::new({
        let block = block.clone();
        tokio::task::spawn_blocking(move || {
            // A proposal from the template updater is built as a decoded block, so it has no
            // wire bytes yet. Encode it once here, off the submission path, so that
            // `PreparedBlockCache::reuse` can match a submission by decoding only its header
            // and comparing the rest of its bytes, without decoding or encoding transactions.
            let wire: Arc<[u8]> = block
                .zcash_serialize_to_vec()
                .expect("verified blocks serialize")
                .into();
            let retained_bytes = contextual.retained_bytes().saturating_add(wire.len());
            (contextual, wire, retained_bytes)
        })
    })
    .await
    .expect("proposal retention accounting uses saturating content-size arithmetic");
    proposals.send_modify(|proposals| {
        // Proposals are validated against the best tip, so entries for other parents can only
        // match a submission that forks the chain, and full verification covers those.
        let parent = block.header.previous_block_hash;
        proposals.retain(|proposal| {
            proposal.key != key && proposal.block.header.previous_block_hash == parent
        });
        proposals.push_front(PreparedProposal {
            key,
            transaction_bytes,
            block,
            contextual,
            wire,
            retained_bytes,
        });
        let mut retained_bytes = 0usize;
        let retained = proposals
            .iter()
            .take(8)
            .take_while(|proposal| {
                retained_bytes = retained_bytes.saturating_add(proposal.retained_bytes);
                retained_bytes <= MAX_PROPOSAL_BYTES
            })
            .count();
        proposals.truncate(retained);
    });
}

/// A decoded submission, optionally carrying its consumed size or an exact proposal match.
///
/// The private match is bound to this immutable block, including its submitted header. It proves
/// byte identity only: the verifier must still check proof of work, time, and live state. A weak
/// handle does not retain contextual data after cache eviction or authorize reuse by another cache.
///
/// Ordinary decoding records only the consumed wire size, not any validation or authorization.
#[derive(Clone, Debug)]
pub struct DecodedBlock {
    pub(super) block: Arc<block::Block>,
    pub(super) consumed_size: Option<usize>,
    pub(super) matched: Option<(Weak<zs::ContextuallyVerifiedBlock>, usize)>,
}

impl DecodedBlock {
    /// Decodes a block with the consensus byte limit, retaining its consumed wire size.
    ///
    /// Trailing bytes are accepted but excluded from the size. All other semantic and contextual
    /// checks still run. Call from a blocking worker.
    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, SerializationError> {
        let input_size = bytes.len();
        let block = Arc::new(block::Block::zcash_deserialize(&mut bytes)?);
        Ok(Self {
            block,
            consumed_size: Some(input_size - bytes.len()),
            matched: None,
        })
    }

    /// Returns the decoded block without exposing or transferring its private evidence.
    pub fn block(&self) -> Arc<block::Block> {
        self.block.clone()
    }
}

impl From<Arc<block::Block>> for DecodedBlock {
    fn from(block: Arc<block::Block>) -> Self {
        Self {
            block,
            consumed_size: None,
            matched: None,
        }
    }
}

impl PartialEq for DecodedBlock {
    fn eq(&self, other: &Self) -> bool {
        self.block == other.block
            && self.consumed_size == other.consumed_size
            && self
                .matched
                .as_ref()
                .map(|(entry, size)| (entry.as_ptr(), size))
                == other
                    .matched
                    .as_ref()
                    .map(|(entry, size)| (entry.as_ptr(), size))
    }
}

impl Eq for DecodedBlock {}

/// Read-only access to successfully validated proposals for decoding mined submissions.
///
/// This shares the verifier's bounded cache, not its validation authority: decoded blocks must
/// still pass the normal verifier, including checks of the submitted proof of work and live state.
#[derive(Clone, Debug)]
pub struct PreparedBlockCache(pub(super) Arc<watch::Sender<ProposalCache>>);

impl PreparedBlockCache {
    /// Decodes a block only if its complete wire bytes match a validated proposal, reusing the
    /// proposal's parsed transactions.
    ///
    /// The transaction section's length and the header with only the nonce and solution
    /// normalized select a candidate, and an exact byte comparison permits reuse. The returned
    /// block keeps the submitted header and a private cache handle for
    /// [`Request::CommitDecoded`](super::Request::CommitDecoded).
    ///
    /// A hit parses the header and compares the transaction bytes, which is cheap enough to run
    /// inline. A miss, an evicted entry or malformed input returns `None` without decoding, so the
    /// caller can decode with [`DecodedBlock::deserialize()`] off the executor.
    pub fn reuse(&self, bytes: &[u8]) -> Option<DecodedBlock> {
        if bytes.len() > usize::try_from(block::MAX_BLOCK_BYTES).expect("block limit fits usize")
            || self.0.borrow().is_empty()
        {
            return None;
        }
        let mut transactions = bytes;
        let header = block::Header::zcash_deserialize(&mut transactions).ok()?;
        let normalized_header = normalized_proposal_header(&header);
        let (block, matched, wire) = {
            let proposals = self.0.borrow();
            let proposal = proposals.iter().find(|proposal| {
                proposal.transaction_bytes == transactions.len()
                    && normalized_proposal_header(&proposal.block.header) == normalized_header
            })?;
            (
                proposal.block.clone(),
                Arc::downgrade(&proposal.contextual),
                proposal.wire.clone(),
            )
        };

        // Candidate selection is not authority to substitute different transaction bytes.
        let header_bytes = wire.len().checked_sub(transactions.len())?;
        if wire.get(header_bytes..)? != transactions {
            return None;
        }
        let block = if *block.header == header {
            block
        } else {
            Arc::new(block::Block {
                header: Arc::new(header),
                transactions: block.transactions.clone(),
            })
        };
        Some(DecodedBlock {
            block,
            consumed_size: None,
            matched: Some((matched, bytes.len())),
        })
    }
}

/// Hash wire bytes with normalized nonce and solution, counting the actual block size in one pass.
pub(super) fn proposal_key_and_size(block: &block::Block) -> ([u8; 32], usize) {
    struct Writer(sha256d::Writer, usize);
    impl Write for Writer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let len = self.0.write(buf)?;
            self.1 += len;
            Ok(len)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    let header = normalized_proposal_header(&block.header);
    let mut writer = Writer(sha256d::Writer::default(), 0);
    header
        .zcash_serialize(&mut writer)
        .expect("hash writers cannot fail");
    block
        .transactions
        .zcash_serialize(&mut writer)
        .expect("hash writers cannot fail");
    // The key always hashes a Common solution, but Regtest's actual wire solution is shorter.
    if matches!(block.header.solution, equihash::Solution::Regtest(_)) {
        writer.1 -=
            equihash::Solution::SERIALIZED_SIZE - equihash::Solution::REGTEST_SERIALIZED_SIZE;
    }
    (writer.0.finish(), writer.1)
}

/// Keep byte-based decoding and parsed proposal lookup on the same normalized header.
pub(super) fn normalized_proposal_header(header: &block::Header) -> block::Header {
    block::Header {
        nonce: [0; 32].into(),
        solution: equihash::Solution::Common([0; 1344]),
        ..*header
    }
}
