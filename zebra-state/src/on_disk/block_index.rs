use std::sync::Arc;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Clone)]
pub(super) struct BlockIndex {
    storage: sled::Db,
}

impl Default for BlockIndex {
    fn default() -> Self {
        let config = sled::Config::default();
        Self {
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

        let by_height = self.storage.open_tree(b"by_height")?;
        let by_hash = self.storage.open_tree(b"by_hash")?;

        let mut bytes = Vec::new();
        block.zcash_serialize(&mut bytes)?;

        // TODO(jlusby): make this transactional
        by_height.insert(&height.0.to_be_bytes(), bytes.as_slice())?;
        by_hash.insert(&hash.0, bytes)?;

        Ok(hash)
    }

    pub(super) fn get(&self, query: impl Into<BlockQuery>) -> Result<Option<Arc<Block>>, Error> {
        let query = query.into();
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
