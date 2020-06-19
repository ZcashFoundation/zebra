use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap},
    error::Error,
    ops::RangeBounds,
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
    pub(super) fn insert(
        &mut self,
        block: impl Into<Arc<Block>>,
    ) -> Result<BlockHeaderHash, Box<dyn Error + Send + Sync + 'static>> {
        let block = block.into();
        let hash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        match self.by_height.entry(height) {
            Entry::Vacant(entry) => {
                let _ = entry.insert(block.clone());
                let _ = self.by_hash.insert(hash, block);
                Ok(hash)
            }
            Entry::Occupied(_) => Err("forks in the chain aren't supported yet")?,
        }
    }

    pub(super) fn get(&mut self, query: impl Into<BlockQuery>) -> Option<Arc<Block>> {
        match query.into() {
            BlockQuery::ByHash(hash) => self.by_hash.get(&hash),
            BlockQuery::ByHeight(height) => self.by_height.get(&height),
        }
        .cloned()
    }

    pub(super) fn get_tip(&self) -> Option<BlockHeaderHash> {
        self.by_height
            .iter()
            .next_back()
            .map(|(_key, value)| value)
            .map(|block| block.as_ref().into())
    }

    pub(super) fn range<R>(&self, range: R) -> Vec<BlockHeaderHash>
    where
        R: RangeBounds<BlockHeight>,
    {
        self.by_height
            .range(range)
            .map(|(_, v)| v)
            .map(|block| block.as_ref().into())
            .collect()
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
