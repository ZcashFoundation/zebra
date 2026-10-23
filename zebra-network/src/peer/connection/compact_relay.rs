//! Opt-in compact transport; consensus and source scoring remain in the ordinary downloader.

use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
    time::Duration,
};

use futures::Sink;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{sleep, timeout, Instant},
};
use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{self, Block},
    chain_tip::ChainTip,
    serialization::{SerializationError, ZcashSerialize},
};

use super::{Connection, Handler, InventoryResponse, State};
use crate::{
    compact::{BlockTransactions, BlockTransactionsRequest, CompactBlock, Reconstruction},
    constants,
    protocol::external::Message,
    BoxError, PeerError, Request, Response,
};

// A decoded body can retain substantially more memory than its two-megabyte wire encoding.
// Six global slots cover three advertised and three received bodies, including detached hash work.
static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(6)));
const RETENTION: Duration = Duration::from_secs(15);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct CompactRelay {
    pub(super) send: bool,
    received: Option<(Arc<CompactBlock>, Instant, OwnedSemaphorePermit)>,
    sent: Option<(Arc<Block>, Instant, OwnedSemaphorePermit)>,
    /// Gossip and sync can request the same body before either verifier commits it.
    reconstructed: Option<(Arc<Block>, Instant, OwnedSemaphorePermit)>,
    last_received: Option<Instant>,
}

impl CompactRelay {
    pub(super) fn expire(&mut self) {
        let now = Instant::now();
        if self
            .received
            .as_ref()
            .is_some_and(|(_, until, _)| *until <= now)
        {
            self.received = None;
        }
        if self
            .sent
            .as_ref()
            .is_some_and(|(_, until, _)| *until <= now)
        {
            self.sent = None;
        }
        if self
            .reconstructed
            .as_ref()
            .is_some_and(|(_, until, _)| *until <= now)
        {
            self.reconstructed = None;
        }
    }

    pub(super) fn has_cached(&self) -> bool {
        self.received.is_some() || self.sent.is_some() || self.reconstructed.is_some()
    }

    pub(super) fn expiry(&self) -> Instant {
        self.received
            .as_ref()
            .map(|(_, until, _)| *until)
            .into_iter()
            .chain(self.sent.as_ref().map(|(_, until, _)| *until))
            .chain(self.reconstructed.as_ref().map(|(_, until, _)| *until))
            .min()
            .unwrap_or_else(|| Instant::now() + RETENTION)
    }
}

impl<S, Tx, C> Connection<S, Tx, C>
where
    S: Service<Request, Response = Response, Error = BoxError>,
    C: ChainTip,
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    /// Query the existing bounded inbound service without turning collision recovery into scoring.
    async fn compact_query(&mut self, request: Request) -> Option<Response> {
        timeout(constants::REQUEST_TIMEOUT, async {
            self.svc.ready().await.ok()?.call(request).await.ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Resolve serving permission anew, so rejected early bodies cannot outlive their admission.
    async fn compact_serving_block(&mut self, hash: block::Hash) -> Option<Arc<Block>> {
        match self
            .compact_query(Request::EarlyBlocksByHash(HashSet::from([hash])))
            .await?
        {
            Response::Blocks(blocks) => blocks.into_iter().find_map(|item| match item {
                InventoryResponse::Available((block, _)) if block.hash() == hash => Some(block),
                _ => None,
            }),
            _ => None,
        }
    }

    pub(super) async fn compact_advert(&mut self, hash: block::Hash) -> Option<Message> {
        if !self.connection_info.early_block_relay || !self.compact.send {
            return None;
        }
        self.compact.expire();
        // Do not replace a body while the peer can still be recovering its transactions.
        if self.compact.sent.is_some() {
            return None;
        }
        let permit = SLOTS.clone().try_acquire_owned().ok()?;
        let block = self.compact_serving_block(hash).await?;
        let (block, compact, permit) = tokio::task::spawn_blocking(move || {
            let compact = CompactBlock::from_block(&block, rand::random());
            let compact = compact
                .ok()
                .filter(|compact| compact.zcash_serialized_size() < block.zcash_serialized_size());
            (block, compact, permit)
        })
        .await
        .ok()?;
        let compact = compact?;
        self.compact.sent = Some((block, Instant::now() + RETENTION, permit));
        Some(Message::CompactBlock(Arc::new(compact)))
    }

    pub(super) async fn receive_compact(&mut self, block: Arc<CompactBlock>) {
        if !self.connection_info.early_block_relay {
            return;
        }
        self.compact.expire();
        let hash = block.header.hash();
        if self.compact.received.is_none()
            && self
                .compact
                .last_received
                .is_none_or(|last| last.elapsed() >= RETENTION)
        {
            if let Ok(permit) = SLOTS.clone().try_acquire_owned() {
                self.compact.last_received = Some(Instant::now());
                self.compact.received = Some((block, Instant::now() + RETENTION, permit));
            }
        }
        // Reconstruction starts only after the downloader applies its global/per-IP admission.
        self.drive_peer_request(Request::AdvertiseBlock(
            hash,
            self.connection_info.connected_addr.get_transient_addr(),
        ))
        .await;
    }

    pub(super) async fn download_block(&mut self, hash: block::Hash) -> Result<Handler, PeerError> {
        self.compact.expire();
        if let Some((block, _, _)) = &self.compact.reconstructed {
            if block.hash() == hash {
                return Ok(self.compact_response(block.clone()));
            }
        }
        if self
            .compact
            .received
            .as_ref()
            .is_some_and(|(block, _, _)| block.header.hash() == hash)
        {
            let (compact, _, permit) = self
                .compact
                .received
                .take()
                .expect("the matching cached compact block exists");
            if let Some(Response::CompactBlockTransactions(candidates)) = self
                .compact_query(Request::CompactBlockTransactions(compact.clone()))
                .await
            {
                let result =
                    tokio::task::spawn_blocking(move || (compact.reconstruct(candidates), permit))
                        .await;
                if let Ok((Ok(reconstruction), permit)) = result {
                    let indexes = reconstruction.missing_indexes();
                    if indexes.is_empty() {
                        if let Some(block) = self
                            .finish_compact(
                                reconstruction,
                                BlockTransactions {
                                    block_hash: hash,
                                    transactions: Vec::new(),
                                },
                                permit,
                            )
                            .await
                        {
                            return Ok(self.compact_response(block));
                        }
                    } else {
                        self.peer_tx
                            .send(Message::GetBlockTransactions(BlockTransactionsRequest {
                                block_hash: hash,
                                indexes,
                            }))
                            .await?;
                        return Ok(Handler::CompactBlock {
                            reconstruction,
                            permit,
                        });
                    }
                }
            }
        }
        self.peer_tx
            .send(Message::GetData(vec![hash.into()]))
            .await?;
        Ok(Handler::BlocksByHash {
            pending_hashes: HashSet::from([hash]),
            blocks: Vec::with_capacity(1),
        })
    }

    fn compact_response(&self, block: Arc<Block>) -> Handler {
        Handler::Finished(Ok(Response::Blocks(vec![InventoryResponse::Available((
            block,
            self.connection_info.connected_addr.get_transient_addr(),
        ))])))
    }

    async fn finish_compact(
        &mut self,
        reconstruction: Reconstruction,
        response: BlockTransactions,
        permit: OwnedSemaphorePermit,
    ) -> Option<Arc<Block>> {
        let (block, permit) =
            tokio::task::spawn_blocking(move || (reconstruction.finish(response), permit))
                .await
                .ok()?;
        let block = block.ok()?;
        // A txid Merkle root does not bind v5/v6 proofs or signatures. Authenticate the ordered
        // authorization root before giving these probabilistically reconstructed bytes to consensus.
        if !matches!(
            self.compact_query(Request::CheckCompactBlock(block.clone()))
                .await,
            Some(Response::Nil)
        ) {
            return None;
        }
        self.compact.reconstructed = Some((block.clone(), Instant::now() + RETENTION, permit));
        Some(block)
    }

    pub(super) async fn receive_compact_transactions(&mut self, response: BlockTransactions) {
        let hash = response.block_hash;
        let State::AwaitingResponse { handler, .. } = &mut self.state else {
            return;
        };
        if !matches!(handler, Handler::CompactBlock { reconstruction, .. } if reconstruction.block_hash() == hash)
        {
            return;
        }
        let Handler::CompactBlock {
            reconstruction,
            permit,
        } = std::mem::replace(handler, Handler::Finished(Ok(Response::Nil)))
        else {
            unreachable!("the matching reconstruction was checked")
        };
        if let Some(block) = self.finish_compact(reconstruction, response, permit).await {
            let response = self.compact_response(block);
            if let State::AwaitingResponse { handler, .. } = &mut self.state {
                *handler = response;
            }
        } else {
            self.fallback_compact(hash).await;
        }
    }

    pub(super) async fn fallback_compact(&mut self, hash: block::Hash) {
        if let Err(error) = self.peer_tx.send(Message::GetData(vec![hash.into()])).await {
            self.fail_with(error).await;
            return;
        }
        if let State::AwaitingResponse { handler, .. } = &mut self.state {
            *handler = Handler::BlocksByHash {
                pending_hashes: HashSet::from([hash]),
                blocks: Vec::with_capacity(1),
            };
            self.request_timer = Some(Box::pin(sleep(constants::REQUEST_TIMEOUT)));
        }
    }

    pub(super) async fn serve_compact_transactions(&mut self, request: BlockTransactionsRequest) {
        if !self.connection_info.early_block_relay || !self.compact.send {
            return;
        }
        self.compact.expire();
        let Some((block, _, _)) = &self.compact.sent else {
            return;
        };
        if block.hash() != request.block_hash {
            return;
        }
        // Only our most recently advertised body can be queried; no arbitrary-block mempool probes.
        if self
            .compact_serving_block(request.block_hash)
            .await
            .is_none()
        {
            self.compact.sent = None;
            return;
        }
        self.compact.expire();
        let Some((block, _, _permit)) = self.compact.sent.take() else {
            return;
        };
        let mut transactions = Vec::with_capacity(request.indexes.len());
        let mut previous = None;
        for index in request.indexes {
            if previous.is_some_and(|previous| index <= previous) {
                return;
            }
            let Some(transaction) = block.transactions.get(index) else {
                return;
            };
            transactions.push(transaction.clone());
            previous = Some(index);
        }
        // Each announcement permits one recovery response, bounded by the original block size.
        if let Err(error) = self
            .peer_tx
            .send(Message::BlockTransactions(BlockTransactions {
                block_hash: request.block_hash,
                transactions,
            }))
            .await
        {
            self.fail_with(error).await;
        }
    }
}

pub(super) fn response_timeout(handler: &Handler) -> Duration {
    if matches!(handler, Handler::CompactBlock { .. }) {
        RECOVERY_TIMEOUT
    } else {
        constants::REQUEST_TIMEOUT
    }
}
