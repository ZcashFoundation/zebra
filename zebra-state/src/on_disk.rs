//! The primary implementation of the `zebra_state::Service` built upon sled
use super::{Request, Response};
use crate::Config;
use futures::prelude::*;
use std::sync::Arc;
use std::{
    error,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, util::BoxService, Service};
use tracing::instrument;
use zebra_chain::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

/// Type alias of our wrapped service
pub type StateService = Buffer<BoxService<Request, Response, Error>, Request>;

#[derive(Clone)]
struct SledState {
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
        let height = block.coinbase_height().unwrap();

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

impl Service<Request> for SledState {
    type Response = Response;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::AddBlock { block } => {
                let mut storage = self.clone();

                // Make sure writes to the state are serialised, by performing
                // them in the state's call.
                // (See the state design RFC #0005 for details.)
                let result = storage.insert(block).map(|hash| Response::Added { hash });

                async { result }.boxed()
            }
            Request::GetBlock { hash } => {
                let storage = self.clone();
                async move {
                    storage
                        .get(hash)?
                        .map(|block| Response::Block { block })
                        .ok_or_else(|| "block could not be found".into())
                }
                .boxed()
            }
            Request::GetTip => {
                let storage = self.clone();
                async move {
                    storage
                        .get_tip()?
                        .map(|hash| Response::Tip { hash })
                        .ok_or_else(|| "zebra-state contains no blocks".into())
                }
                .boxed()
            }
            Request::GetDepth { hash } => {
                let storage = self.clone();

                async move {
                    if !storage.contains(&hash)? {
                        return Ok(Response::Depth(None));
                    }

                    let block = storage
                        .get(hash)?
                        .expect("block must be present if contains returned true");
                    let tip_hash = storage
                        .get_tip()?
                        .expect("storage must have a tip if it contains the previous block");
                    let tip = storage
                        .get(tip_hash)?
                        .expect("block must be present if contains returned true");

                    let depth =
                        tip.coinbase_height().unwrap().0 - block.coinbase_height().unwrap().0;

                    Ok(Response::Depth(Some(depth)))
                }
                .boxed()
            }
            Request::GetBlockLocator { genesis } => {
                let storage = self.clone();

                async move {
                    let tip_hash = match storage.get_tip()? {
                        Some(tip) => tip,
                        None => {
                            return Ok(Response::BlockLocator {
                                block_locator: vec![genesis],
                            })
                        }
                    };

                    let tip = storage
                        .get(tip_hash)?
                        .expect("block must be present if contains returned true");

                    let tip_height = tip
                        .coinbase_height()
                        .expect("tip of the current chain will have a coinbase height");

                    let heights = crate::block_locator_heights(tip_height);

                    let block_locator = heights
                        .map(|height| {
                            storage.get_main_chain_at(height).map(|hash| {
                                hash.expect("there should be no holes in the current chain")
                            })
                        })
                        .collect::<Result<_, _>>()?;

                    Ok(Response::BlockLocator { block_locator })
                }
                .boxed()
            }
        }
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

/// Returns a type that implements the `zebra_state::Service` using `sled`.
///
/// Each `network` has its own separate sled database.
pub fn init(config: Config, network: Network) -> StateService {
    Buffer::new(BoxService::new(SledState::new(&config, network)), 1)
}
