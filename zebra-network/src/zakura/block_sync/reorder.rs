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

    pub(super) fn contains(&self, height: block::Height) -> bool {
        self.blocks.contains_key(&height)
    }

    pub(super) fn insert(
        &mut self,
        height: block::Height,
        block: Arc<block::Block>,
        bytes: u64,
        budget: &mut ByteBudget,
    ) -> ReorderInsertResult {
        if self.blocks.contains_key(&height) {
            return ReorderInsertResult::Duplicate;
        }
        if !budget.try_reserve(bytes) {
            return ReorderInsertResult::BudgetFull;
        }

        self.blocks.insert(height, BufferedBlock { block, bytes });
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
        ReorderInsertResult::Inserted
    }

    pub(super) fn drain_contiguous_prefix(
        &mut self,
        verified_block_tip: block::Height,
        budget: &mut ByteBudget,
    ) -> Vec<(block::Height, Arc<block::Block>)> {
        let mut released = Vec::new();
        let mut next = match next_height(verified_block_tip) {
            Some(next) => next,
            None => return released,
        };

        while let Some(buffered) = self.blocks.remove(&next) {
            self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered.bytes);
            budget.release(buffered.bytes);
            released.push((next, buffered.block));
            let Some(after) = next_height(next) else {
                break;
            };
            next = after;
        }

        released
    }

    pub(crate) fn clear(&mut self, budget: &mut ByteBudget) {
        self.drop_from(block::Height::MIN, budget);
    }

    pub(crate) fn drop_from(&mut self, from: block::Height, budget: &mut ByteBudget) {
        let heights: Vec<_> = self
            .blocks
            .range(from..)
            .map(|(height, _)| *height)
            .collect();
        for height in heights {
            if let Some(buffered) = self.blocks.remove(&height) {
                self.buffered_bytes = self.buffered_bytes.saturating_sub(buffered.bytes);
                budget.release(buffered.bytes);
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ReorderInsertResult {
    Inserted,
    Duplicate,
    BudgetFull,
}

#[derive(Clone, Debug)]
struct BufferedBlock {
    block: Arc<block::Block>,
    bytes: u64,
}
