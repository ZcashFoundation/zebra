//! The primary implementation of the `zebra_state::Service` built upon sled
use crate::Config;
use std::error;
use std::sync::Arc;
use tracing::instrument;
use zebra_chain::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

#[derive(Clone)]
pub struct SledState {
    storage: sled::Db,
}

impl SledState {
    #[instrument]
    pub(crate) fn new(config: &Config, network: Network) -> Self {
        let config = config.sled_config(network);

        Self {
            storage: config.open().unwrap(),
        }
    }

    #[instrument(skip(self))]
    pub(super) fn insert(
        &mut self,
        block: impl Into<Arc<Block>> + std::fmt::Debug,
    ) -> Result<block::Hash, Error> {
        use sled::Transactional;

        let block = block.into();
        let hash = block.hash();
        let height = block
            .coinbase_height()
            .expect("missing height: valid blocks must have a height");

        // Make sure blocks are inserted in order, as a defence in depth.
        // See the state design RFC #0005 for details.
        //
        // TODO: handle multiple chains
        match self.get_tip()? {
            None => {
                // This is a defence in depth - there is no need to check the
                // genesis hash or previous block hash here.
                assert_eq!(
                    height,
                    block::Height(0),
                    "out of order block: the first block must be at the genesis height"
                );
            }
            Some(tip_hash) => {
                assert_eq!(
                    block.header.previous_block_hash, tip_hash,
                    "out of order block: the next block must be a child of the current tip"
                );
                let tip_block = self
                    .get(tip_hash)?
                    .expect("missing tip block: tip hashes must have a corresponding block");
                let tip_height = tip_block
                    .coinbase_height()
                    .expect("missing height: valid blocks must have a height");
                assert_eq!(
                    height,
                    block::Height(tip_height.0 + 1),
                    "out of order block: the next height must be 1 greater than the tip height"
                );
            }
        };

        let height_map = self.storage.open_tree(b"height_map")?;
        let by_hash = self.storage.open_tree(b"by_hash")?;

        let bytes = block.zcash_serialize_to_vec()?;

        (&height_map, &by_hash).transaction(|(height_map, by_hash)| {
            height_map.insert(&height.0.to_be_bytes(), &hash.0)?;
            by_hash.insert(&hash.0, bytes.clone())?;
            Ok(())
        })?;

        tracing::trace!(?height, ?hash, "Committed block");
        metrics::gauge!("state.committed.block.height", height.0 as _);
        metrics::counter!("state.committed.block.count", 1);

        Ok(hash)
    }

    #[instrument(skip(self))]
    pub(super) fn get(&self, hash: block::Hash) -> Result<Option<Arc<Block>>, Error> {
        let by_hash = self.storage.open_tree(b"by_hash")?;
        let key = &hash.0;
        let value = by_hash.get(key)?;

        if let Some(bytes) = value {
            let bytes = bytes.as_ref();
            let block = ZcashDeserialize::zcash_deserialize(bytes)?;
            Ok(Some(block))
        } else {
            Ok(None)
        }
    }

    #[instrument(skip(self))]
    pub(super) fn get_main_chain_at(
        &self,
        height: block::Height,
    ) -> Result<Option<block::Hash>, Error> {
        let height_map = self.storage.open_tree(b"height_map")?;
        let key = height.0.to_be_bytes();
        let value = height_map.get(key)?;

        if let Some(bytes) = value {
            let bytes = bytes.as_ref();
            let hash = ZcashDeserialize::zcash_deserialize(bytes)?;
            Ok(Some(hash))
        } else {
            Ok(None)
        }
    }

    #[instrument(skip(self))]
    pub(super) fn get_tip(&self) -> Result<Option<block::Hash>, Error> {
        let tree = self.storage.open_tree(b"height_map")?;
        let last_entry = tree.iter().values().next_back();

        match last_entry {
            Some(Ok(bytes)) => Ok(Some(ZcashDeserialize::zcash_deserialize(bytes.as_ref())?)),
            Some(Err(e)) => Err(e)?,
            None => Ok(None),
        }
    }

    #[instrument(skip(self))]
    fn contains(&self, hash: &block::Hash) -> Result<bool, Error> {
        let by_hash = self.storage.open_tree(b"by_hash")?;
        let key = &hash.0;

        Ok(by_hash.contains_key(key)?)
    }
}

/// An alternate repr for `block::Height` that implements `AsRef<[u8]>` for usage
/// with sled
struct BytesHeight(u32, [u8; 4]);

impl From<block::Height> for BytesHeight {
    fn from(height: block::Height) -> Self {
        let bytes = height.0.to_be_bytes();
        Self(height.0, bytes)
    }
}

impl AsRef<[u8]> for BytesHeight {
    fn as_ref(&self) -> &[u8] {
        &self.1[..]
    }
}

pub(super) enum BlockQuery {
    ByHash(block::Hash),
    ByHeight(block::Height),
}

impl From<block::Hash> for BlockQuery {
    fn from(hash: block::Hash) -> Self {
        Self::ByHash(hash)
    }
}

impl From<block::Height> for BlockQuery {
    fn from(height: block::Height) -> Self {
        Self::ByHeight(height)
    }
}

type BoxError = Box<dyn error::Error + Send + Sync + 'static>;

// these hacks are necessary to capture spantraces that can be extracted again
// while still having a nice From impl.
//
// Please forgive me.

/// a type that can store any error and implements the Error trait at the cost of
/// not implementing From<E: Error>
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct BoxRealError(BoxError);

/// The TracedError wrapper on a type that implements Error
#[derive(Debug)]
pub struct Error(tracing_error::TracedError<BoxRealError>);

macro_rules! impl_from {
    ($($src:ty,)*) => {$(
        impl From<$src> for Error {
            fn from(source: $src) -> Self {
                let source = BoxRealError(source.into());
                Self(source.into())
            }
        }
    )*
    }
}

// The hoops we have to jump through to keep using this like a BoxError
impl_from! {
    &str,
    SerializationError,
    std::io::Error,
    sled::Error,
    sled::transaction::TransactionError,
}

impl Into<BoxError> for Error {
    fn into(self) -> BoxError {
        BoxError::from(self.0)
    }
}
