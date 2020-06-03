use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};
#[derive(Default)]
pub(super) struct BlockIndex {
    by_hash: HashMap<BlockHeaderHash, Arc<Block>>,
    by_height: BTreeMap<BlockHeight, Arc<Block>>,
}

impl BlockIndex {
    pub(super) fn insert(&mut self, block: impl Into<Arc<Block>>) {
        let block = block.into();
        let hash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        assert!(
            self.by_hash.insert(hash, block.clone()).is_none(),
            "blocks shouldn't have the same hash"
        );
        assert!(
            self.by_height.insert(height, block).is_none(),
            "blocks with the same height are currently unsupported"
        );
    }

    pub(super) fn get(&mut self, query: impl Into<BlockQuery>) -> Option<Arc<Block>> {
        match query.into() {
            BlockQuery::ByHash(hash) => self.by_hash.get(&hash),
            BlockQuery::ByHeight(height) => self.by_height.get(&height),
        }
        .cloned()
    }

    pub(super) fn tip(&self) -> Option<Arc<Block>> {
        self.by_height
            .iter()
            .next_back()
            .map(|(_key, value)| value)
            .cloned()
    }
}

pub(super) enum BlockQuery {
    ByHash(BlockHeaderHash),
    ByHeight(BlockHeight),
}

impl From<BlockHeaderHash> for BlockQuery {
    fn from(hash: BlockHeaderHash) -> Self {
        Self::ByHash(hash)
    }
}

impl From<BlockHeight> for BlockQuery {
    fn from(height: BlockHeight) -> Self {
        Self::ByHeight(height)
    }
}
