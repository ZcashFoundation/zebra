use super::{state::*, *};

#[derive(Clone, Debug)]
pub(crate) struct ReorderBuffer {
    blocks: BTreeMap<block::Height, BufferedBlock>,
    buffered_bytes: u64,
}

impl ReorderBuffer {
    pub(super) fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            buffered_bytes: 0,
        }
    }

    pub(super) fn buffered_bytes(&self) -> u64 {
        self.buffered_bytes
    }

    pub(super) fn len(&self) -> usize {
        self.blocks.len()
    }

    pub(super) fn contains(&self, height: block::Height) -> bool {
        self.blocks.contains_key(&height)
    }

    pub(super) fn contains_at_or_above(&self, height: block::Height) -> bool {
        self.blocks.range(height..).next().is_some()
    }

    pub(super) fn hash(&self, height: block::Height) -> Option<block::Hash> {
        self.blocks
            .get(&height)
            .map(|buffered| buffered.block.hash())
    }

    /// Buffer a received body that already owns its `bytes` reservation.
    ///
    /// The caller reserved worst-case bytes for this height at send time and
    /// shrank that reservation to `bytes` on receipt, so the reorder buffer takes
    /// ownership of the existing reservation without touching the budget and can
    /// never fail on budget. A `Duplicate` height is left to the caller to release.
    pub(super) fn insert(
        &mut self,
        height: block::Height,
        block: Arc<block::Block>,
        bytes: u64,
        source_peer: ZakuraPeerId,
    ) -> ReorderInsertResult {
        if self.blocks.contains_key(&height) {
            return ReorderInsertResult::Duplicate;
        }

        self.blocks.insert(
            height,
            BufferedBlock {
                block,
                bytes,
                source_peer,
            },
        );
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
        ReorderInsertResult::Inserted
    }

    pub(super) fn drain_contiguous_prefix(
        &mut self,
        verified_block_tip: block::Height,
    ) -> Vec<(block::Height, Arc<block::Block>, u64, ZakuraPeerId)> {
        let mut released = Vec::new();
        let mut next = match next_height(verified_block_tip) {
            Some(next) => next,
            None => return released,
        };

        while let Some(buffered) = self.blocks.remove(&next) {
            self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered.bytes);
            released.push((next, buffered.block, buffered.bytes, buffered.source_peer));
            let Some(after) = next_height(next) else {
                break;
            };
            next = after;
        }

        released
    }

    /// Drop every buffered body and return the total bytes they held, so the
    /// caller releases exactly that reservation. The reorder buffer is owned by
    /// the `Sequencer`, which does not touch the byte budget; it returns the
    /// freed bytes to the reactor instead.
    pub(crate) fn clear(&mut self) -> u64 {
        self.drop_from(block::Height::MIN)
    }

    /// Drop buffered bodies at or below `through` and return the bytes they held.
    pub(crate) fn drop_through(&mut self, through: block::Height) -> u64 {
        let heights: Vec<_> = self
            .blocks
            .range(..=through)
            .map(|(height, _)| *height)
            .collect();
        let mut released = 0u64;
        for height in heights {
            if let Some(buffered) = self.blocks.remove(&height) {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered.bytes);
                released = released.saturating_add(buffered.bytes);
            }
        }
        released
    }

    /// Drop buffered bodies at or above `from` and return the bytes they held.
    pub(crate) fn drop_from(&mut self, from: block::Height) -> u64 {
        let heights: Vec<_> = self
            .blocks
            .range(from..)
            .map(|(height, _)| *height)
            .collect();
        let mut released = 0u64;
        for height in heights {
            if let Some(buffered) = self.blocks.remove(&height) {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered.bytes);
                released = released.saturating_add(buffered.bytes);
            }
        }
        released
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ReorderInsertResult {
    Inserted,
    Duplicate,
}

#[derive(Clone, Debug)]
struct BufferedBlock {
    block: Arc<block::Block>,
    bytes: u64,
    /// The peer that delivered this body, so an apply rejection can be attributed
    /// back to it for misbehavior scoring.
    source_peer: ZakuraPeerId,
}
