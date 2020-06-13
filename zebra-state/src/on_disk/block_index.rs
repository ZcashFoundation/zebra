use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap},
    sync::{Arc, Weak},
};
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

pub(super) struct BlockIndex {
    cache: WeakCache,
    storage: sled::Db,
}

impl Default for BlockIndex {
    fn default() -> Self {
        let config = sled::Config::default();
        Self {
            cache: Default::default(),
            storage: config.open().unwrap(),
        }
    }
}

impl BlockIndex {
    pub(super) fn insert(
        &mut self,
        block: impl Into<Arc<Block>>,
    ) -> Result<BlockHeaderHash, Error> {
        let block = block.into();
        let hash: BlockHeaderHash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        // make sure we can insert into the cache first
        self.cache.insert(block.clone())?;

        let by_height = self.storage.open_tree(b"by_height")?;
        let by_hash = self.storage.open_tree(b"by_hash")?;

        let mut bytes = Vec::new();
        block.zcash_serialize(&mut bytes)?;

        // TODO(jlusby): make this transactional
        by_height.insert(&height.0.to_le_bytes(), bytes.as_slice())?;
        by_hash.insert(&hash.0, bytes)?;

        Ok(hash)
    }

    pub(super) fn get(
        &mut self,
        query: impl Into<BlockQuery>,
    ) -> Result<Option<Arc<Block>>, Error> {
        self.cache
            .get(query)
            .map(Some)
            .or_else(|query| self.query_get(query))
    }

    fn query_get(&mut self, query: BlockQuery) -> Result<Option<Arc<Block>>, Error> {
        let block = match query {
            BlockQuery::ByHash(hash) => self.storage.open_tree(b"by_hash")?.get(&hash.0),
            BlockQuery::ByHeight(height) => self
                .storage
                .open_tree(b"by_height")?
                .get(&height.0.to_le_bytes()),
        }?
        .map(|bytes| ZcashDeserialize::zcash_deserialize(bytes.as_ref()))
        .transpose()?;

        Ok(block)
    }

    pub(super) fn get_tip(&self) -> Result<Option<BlockHeaderHash>, Error> {
        if let Some(tip) = self.cache.get_tip() {
            return Ok(Some(tip));
        }

        self.tip_from_storage()
    }

    fn tip_from_storage(&self) -> Result<Option<BlockHeaderHash>, Error> {
        Ok(self
            .storage
            .open_tree(b"by_height")?
            .iter()
            .next_back()
            .transpose()?
            .map(|(_key, val)| val)
            .map(|bytes| Arc::<Block>::zcash_deserialize(bytes.as_ref()))
            .transpose()?
            .map(|block| block.as_ref().into()))
    }
}

#[derive(Default)]
struct WeakCache {
    by_hash: HashMap<BlockHeaderHash, Weak<Block>>,
    by_height: BTreeMap<BlockHeight, Weak<Block>>,
}

impl WeakCache {
    pub(super) fn insert(
        &mut self,
        block: impl Into<Arc<Block>>,
    ) -> Result<BlockHeaderHash, Error> {
        let block = block.into();
        let hash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        match self.by_height.entry(height) {
            Entry::Vacant(entry) => {
                let _ = entry.insert(Arc::downgrade(&block));
                let _ = self.by_hash.insert(hash, Arc::downgrade(&block));
                Ok(hash)
            }
            Entry::Occupied(_) => Err("forks in the chain aren't supported yet")?,
        }
    }

    pub(super) fn get(&mut self, query: impl Into<BlockQuery>) -> Result<Arc<Block>, BlockQuery> {
        let query = query.into();
        match &query {
            BlockQuery::ByHash(hash) => self.by_hash.get(&hash),
            BlockQuery::ByHeight(height) => self.by_height.get(&height),
        }
        .and_then(Weak::upgrade)
        .ok_or(query)
    }

    pub(super) fn get_tip(&self) -> Option<BlockHeaderHash> {
        self.by_height
            .iter()
            .next_back()
            .map(|(_key, value)| value)
            .and_then(Weak::upgrade)
            .map(|block| block.as_ref().into())
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
