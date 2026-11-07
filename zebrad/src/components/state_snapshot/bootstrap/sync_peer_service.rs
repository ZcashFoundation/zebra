//! A connected version 2 peer of the node's network, as the bootstrap's peer service.
//!
//! The bootstrap normally dials its peers from endpoints of its own. A node
//! that finds its snapshot peers among its connected version 2 peers uses
//! those connections instead: the peers usually allow one connection per IP
//! address, so they would refuse a second one.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tower::Service;

use zebra_chain::{
    block::{self, Block},
    serialization::ZcashDeserialize,
};
use zebra_network::{
    self as zn,
    sync_peer::{GetBlockRange, ObjectChunk, RangeItem},
    InventoryResponse, SyncPeer,
};

use crate::BoxError;

/// Answers the bootstrap's `get-object` and block requests over a [`SyncPeer`]:
/// [`zn::Request::Object`] with `get-object`, and [`zn::Request::BlocksByHash`]
/// for one block with a one-block `get-block-range`. Other requests fail.
#[derive(Clone, Debug)]
pub(crate) struct SyncPeerService {
    peer: SyncPeer,
}

impl SyncPeerService {
    /// Returns a service for `peer`.
    pub(crate) fn new(peer: SyncPeer) -> Self {
        SyncPeerService { peer }
    }
}

impl Service<zn::Request> for SyncPeerService {
    type Response = zn::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<zn::Response, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.peer.is_closed() {
            Poll::Ready(Err("the version 2 connection closed".into()))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, request: zn::Request) -> Self::Future {
        let peer = self.peer.clone();
        Box::pin(async move {
            match request {
                zn::Request::Object {
                    hash,
                    offset,
                    length,
                } => match peer.get_object(hash, offset, length).await? {
                    ObjectChunk::Found { size, data } => Ok(zn::Response::Object {
                        size,
                        data: data.to_vec(),
                    }),
                    ObjectChunk::NotFound => Ok(zn::Response::ObjectNotFound),
                },
                zn::Request::BlocksByHash(hashes) if hashes.len() == 1 => {
                    let hash = *hashes.iter().next().expect("the set has one hash");
                    block(&peer, hash).await
                }
                request => Err(format!("a sync peer connection does not serve {request:?}").into()),
            }
        })
    }
}

/// Requests the block `hash` from `peer` with a one-block `get-block-range`.
async fn block(peer: &SyncPeer, hash: block::Hash) -> Result<zn::Response, BoxError> {
    let mut range = peer
        .get_block_range(GetBlockRange {
            final_hash: hash,
            count: 1,
            max_bytes: 0,
        })
        .await?;

    let block = match range.next().await {
        Some(Ok(RangeItem::Block { raw, .. })) => Arc::new(Block::zcash_deserialize(&raw[..])?),
        Some(Ok(RangeItem::End { .. })) | None => {
            return Ok(zn::Response::Blocks(vec![InventoryResponse::Missing(hash)]))
        }
        Some(Err(error)) => return Err(error.into()),
    };
    if block.hash() != hash {
        return Err(format!("the peer served block {} for {hash}", block.hash()).into());
    }

    Ok(zn::Response::Blocks(vec![InventoryResponse::Available((
        block,
        Some(peer.addr()),
    ))]))
}
