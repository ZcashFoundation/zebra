//! The primary implementation of the `zebra_state::Service` built upon sled
use super::{Request, Response};
use crate::Config;
use future::Either;
use futures::prelude::*;
use sled::Tree;
use std::sync::Arc;
use std::{
    collections::HashMap,
    error,
    future::Future,
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::broadcast;
use tower::buffer::Buffer;
use tracing::instrument;
use zebra_chain::serialization::{
    SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    transaction::{OutPoint, TransparentOutput},
    types::BlockHeight,
    Network,
};

#[allow(dead_code)]
struct Service {
    storage: SledState,
    pending_utxo: SledMap<OutPoint, TransparentOutput>,
}

impl Service {
    fn new(config: &Config, network: Network) -> Self {
        let storage = SledState::new(config, network);

        Self {
            pending_utxo: storage.new_map("by_outpoint").unwrap(),
            storage,
        }
    }
}

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

    fn new_map<K, V>(&self, name: impl AsRef<[u8]>) -> Result<SledMap<K, V>, Error>
    where
        K: AsRef<[u8]> + Eq + Hash,
        V: ZcashDeserialize + ZcashSerialize + Clone,
    {
        let tree = self.storage.open_tree(name)?;
        Ok(SledMap::new(tree, Default::default()))
    }

    #[instrument(skip(self))]
    pub(super) fn insert(
        &mut self,
        block: impl Into<Arc<Block>> + std::fmt::Debug,
    ) -> Result<BlockHeaderHash, Error> {
        let block = block.into();
        let hash: BlockHeaderHash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        let height_map = self.storage.open_tree(b"height_map")?;
        let by_hash = self.storage.open_tree(b"by_hash")?;

        let bytes = block.zcash_serialize_to_vec()?;

        // TODO(jlusby): make this transactional
        height_map.insert(&height.0.to_be_bytes(), &hash.0)?;
        by_hash.insert(&hash.0, bytes)?;

        Ok(hash)
    }

    #[instrument(skip(self))]
    pub(super) fn get(&self, hash: BlockHeaderHash) -> Result<Option<Arc<Block>>, Error> {
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
        height: BlockHeight,
    ) -> Result<Option<BlockHeaderHash>, Error> {
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
    pub(super) fn get_tip(&self) -> Result<Option<BlockHeaderHash>, Error> {
        let tree = self.storage.open_tree(b"height_map")?;
        let last_entry = tree.iter().values().next_back();

        match last_entry {
            Some(Ok(bytes)) => Ok(Some(ZcashDeserialize::zcash_deserialize(bytes.as_ref())?)),
            Some(Err(e)) => Err(e)?,
            None => Ok(None),
        }
    }

    #[instrument(skip(self))]
    fn contains(&self, hash: &BlockHeaderHash) -> Result<bool, Error> {
        let by_hash = self.storage.open_tree(b"by_hash")?;
        let key = &hash.0;

        Ok(by_hash.contains_key(key)?)
    }

    /// Return a utxo if it exists.
    ///
    /// # Details
    ///
    /// Returns an error if the UTXO has been spent and returns `Ok(None)` if the
    /// UTXO is unknown.
    ///
    fn get_utxo(
        &self,
        _outpoint: OutPoint,
    ) -> Result<Either<TransparentOutput, broadcast::Receiver<TransparentOutput>>, Error> {
        // if contains outpoint, return outpoint, if not, check if we have a
        // channel for it, if so subscribe and return the receiver, if not,
        // create a channel, insert, and return the receiver
        todo!()
    }
}

impl tower::Service<Request> for Service {
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
                let mut storage = self.storage.clone();

                async move { storage.insert(block).map(|hash| Response::Added { hash }) }.boxed()
            }
            Request::GetBlock { hash } => {
                let storage = self.storage.clone();
                async move {
                    storage
                        .get(hash)?
                        .map(|block| Response::Block { block })
                        .ok_or_else(|| "block could not be found".into())
                }
                .boxed()
            }
            Request::GetTip => {
                let storage = self.storage.clone();
                async move {
                    storage
                        .get_tip()?
                        .map(|hash| Response::Tip { hash })
                        .ok_or_else(|| "zebra-state contains no blocks".into())
                }
                .boxed()
            }
            Request::GetDepth { hash } => {
                let storage = self.storage.clone();

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
                let storage = self.storage.clone();

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
            Request::GetUTXO { outpoint } => match self.storage.get_utxo(outpoint) {
                Ok(Either::Left(output)) => async move { Ok(Response::UTXO { output }) }.boxed(),
                Ok(Either::Right(mut rx)) => async move {
                    let output = rx.recv().await?;
                    Ok(Response::UTXO { output })
                }
                .boxed(),
                Err(e) => async move { Err(e) }.boxed(),
            },
        }
    }
}

/// An alternate repr for `BlockHeight` that implements `AsRef<[u8]>` for usage
/// with sled
struct BytesHeight(u32, [u8; 4]);

impl From<BlockHeight> for BytesHeight {
    fn from(height: BlockHeight) -> Self {
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

/// Returns a type that implements the `zebra_state::Service` using `sled`.
///
/// Each `network` has its own separate sled database.
pub fn init(
    config: Config,
    network: Network,
) -> impl tower::Service<
    Request,
    Response = Response,
    Error = BoxError,
    Future = impl Future<Output = Result<Response, BoxError>>,
> + Send
       + Clone
       + 'static {
    Buffer::new(Service::new(&config, network), 1)
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
struct Error(tracing_error::TracedError<BoxRealError>);

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
    broadcast::RecvError,
}

impl Into<BoxError> for Error {
    fn into(self) -> BoxError {
        BoxError::from(self.0)
    }
}

/// A map like interface ontop of sled with a future based interface for
/// retrieving values that aren't available yet.
#[allow(dead_code)]
struct SledMap<K, V> {
    tree: Tree,
    pending: HashMap<K, broadcast::Sender<V>>,
}

#[allow(clippy::manual_async_fn)]
#[allow(dead_code, unused_variables)]
impl<K, V> SledMap<K, V>
where
    K: AsRef<[u8]> + Eq + Hash,
    V: ZcashDeserialize + ZcashSerialize + Clone,
{
    fn new(tree: Tree, pending: HashMap<K, broadcast::Sender<V>>) -> Self {
        Self { tree, pending }
    }

    fn insert(&mut self, key: K, value: V) -> impl Future<Output = Option<V>> {
        async { todo!() }
    }

    fn get(&mut self, k: K) -> impl Future<Output = Option<V>> {
        // if contains outpoint, return outpoint, if not, check if we have a
        // channel for it, if so subscribe and return the receiver, if not,
        // create a channel, insert, and return the receiver
        let val = self
            .tree
            .get(&k)
            .unwrap()
            .map(|bytes| bytes.zcash_deserialize_into().unwrap())
            .map(Either::Left)
            .unwrap_or_else(|| {
                Either::Right(
                    self.pending
                        .get(&k)
                        .map(|sender| sender.subscribe())
                        .unwrap_or_else(|| {
                            let (tx, rx) = broadcast::channel(1);
                            self.pending.insert(k, tx);
                            rx
                        }),
                )
            });

        async {
            match val {
                Either::Left(val) => Some(val),
                Either::Right(mut rx) => Some(rx.recv().await.unwrap()),
            }
        }
    }
}
