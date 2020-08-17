use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap},
    error::Error,
    sync::Arc,
};
use zebra_chain::block::{self, Block};
#[derive(Default)]
pub(super) struct BlockIndex {
    by_hash: HashMap<block::Hash, Arc<Block>>,
    height_map: BTreeMap<block::Height, block::Hash>,
}

impl BlockIndex {
    pub(super) fn insert(
        &mut self,
        block: impl Into<Arc<Block>>,
    ) -> Result<block::Hash, Box<dyn Error + Send + Sync + 'static>> {
        let block = block.into();
        let hash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        match self.height_map.entry(height) {
            Entry::Vacant(entry) => {
                let _ = entry.insert(hash);
                let _ = self.by_hash.insert(hash, block);
                Ok(hash)
            }
            Entry::Occupied(_) => Err("forks in the chain aren't supported yet")?,
        }
    }

    pub(super) fn get(&self, hash: block::Hash) -> Option<Arc<Block>> {
        self.by_hash.get(&hash).cloned()
    }

    pub(super) fn get_main_chain_at(&self, height: block::Height) -> Option<block::Hash> {
        self.height_map.get(&height).cloned()
    }

    pub(super) fn get_tip(&self) -> Option<Arc<Block>> {
        self.height_map.iter().next_back().map(|(_height, &hash)| {
            self.get(hash)
                .expect("block must be in pool to be in the height map")
        })
    }
}
