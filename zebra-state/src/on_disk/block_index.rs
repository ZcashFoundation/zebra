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
        by_height.insert(&height.0.to_be_bytes(), bytes.as_slice())?;
        by_hash.insert(&hash.0, bytes)?;

        Ok(hash)
    }

    pub(super) fn get(
        &mut self,
        query: impl Into<BlockQuery>,
    ) -> Result<Option<Arc<Block>>, Error> {
        let query = query.into();

        if let Some(block) = self.cache.get(&query) {
            Ok(Some(block))
        } else {
            self.query_block(query)
        }
    }

    fn query_block(&mut self, query: BlockQuery) -> Result<Option<Arc<Block>>, Error> {
        let value = match query {
            BlockQuery::ByHash(hash) => {
                let by_hash = self.storage.open_tree(b"by_hash")?;
                let key = &hash.0;
                by_hash.get(key)?
            }
            BlockQuery::ByHeight(height) => {
                let by_height = self.storage.open_tree(b"by_height")?;
                let key = height.0.to_be_bytes();
                by_height.get(key)?
            }
        };

        if let Some(bytes) = value {
            let bytes = bytes.as_ref();
            let block = ZcashDeserialize::zcash_deserialize(bytes)?;
            Ok(Some(block))
        } else {
            Ok(None)
        }
    }

    pub(super) fn get_tip(&self) -> Result<Option<BlockHeaderHash>, Error> {
        if let Some(tip) = self.cache.get_tip() {
            return Ok(Some(tip));
        }

        self.tip_from_storage()
    }

    fn tip_from_storage(&self) -> Result<Option<BlockHeaderHash>, Error> {
        let tree = self.storage.open_tree(b"by_height")?;
        let last_entry = tree.iter().values().next_back();

        match last_entry {
            Some(Ok(bytes)) => {
                let block = Arc::<Block>::zcash_deserialize(bytes.as_ref())?;
                Ok(Some(block.as_ref().into()))
            }
            Some(Err(e)) => Err(e)?,
            None => Ok(None),
        }
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

    pub(super) fn get(&mut self, query: &BlockQuery) -> Option<Arc<Block>> {
        match query {
            BlockQuery::ByHash(hash) => self.by_hash.get(&hash),
            BlockQuery::ByHeight(height) => self.by_height.get(&height),
        }
        .and_then(Weak::upgrade)
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
