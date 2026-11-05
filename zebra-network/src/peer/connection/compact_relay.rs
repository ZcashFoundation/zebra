//! Opt-in compact transport; consensus and source scoring remain in the ordinary downloader.

use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
    time::Duration,
};

use futures::Sink;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{sleep, timeout, timeout_at, Instant},
};
use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{self, Block, MAX_BLOCK_BYTES},
    chain_tip::ChainTip,
    serialization::{CompactSizeMessage, SerializationError, ZcashSerialize},
};
use zebra_node_services::compact::ReconstructedBlock;

use super::{block_hashes, is_block_inventory, Connection, Handler, InventoryResponse, State};
use crate::{
    compact::{BlockTransactions, BlockTransactionsRequest, CompactBlock, Reconstruction},
    constants,
    protocol::external::{types::Nonce, Message},
    BlockAuthorization, BoxError, PeerError, Request, Response,
};

// A decoded body can retain substantially more memory than its two-megabyte wire encoding.
// Six global slots cover three advertised and three received bodies, including detached hash work.
// Received bodies must pass proof-of-work and current-best-tip screening before taking a slot.
static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(6)));
const RETENTION: Duration = Duration::from_secs(15);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);

struct ServingBlock {
    block: Arc<Block>,
    /// Keep the sent short-ID keys reproducible without retaining the compact transaction vectors.
    nonce: u64,
    authorization: Option<BlockAuthorization>,
}

impl ServingBlock {
    fn is_authorized(&self) -> bool {
        self.authorization
            .as_ref()
            .is_none_or(|authorization| authorization.is_authorized(&self.block))
    }
}

struct Discovery {
    parent: block::Hash,
    hashes: Vec<block::Hash>,
    until: Instant,
}

struct DiscoveryBarrier {
    nonce: Nonce,
    parent: block::Hash,
    hash: block::Hash,
    deadline: Instant,
}

#[derive(Default)]
pub(super) struct CompactRelay {
    pub(super) send: bool,
    received: Option<(Arc<CompactBlock>, Instant, OwnedSemaphorePermit)>,
    sent: Option<(ServingBlock, Option<Instant>, OwnedSemaphorePermit)>,
    /// Bound aggregate recovery payloads, including repeated or empty requests.
    recovery_bytes_left: usize,
    /// Gossip and sync can request the same body before either verifier commits it.
    reconstructed: Option<(ReconstructedBlock, Instant, OwnedSemaphorePermit)>,
    last_received: Option<Instant>,
    /// A screened announcement or useful retired reply can answer discovery once, not for a TTL.
    discovery: Option<Discovery>,
    /// Separate from heartbeat pings: only its matching pong retires the old wire request.
    discovery_barrier: Option<DiscoveryBarrier>,
}

impl CompactRelay {
    pub(super) fn clear_sent(&mut self) {
        self.sent = None;
    }

    pub(super) fn advert_is_authorized(&self) -> bool {
        self.sent
            .as_ref()
            .is_some_and(|(serving, until, _)| until.is_none() && serving.is_authorized())
    }

    pub(super) fn mark_sent(&mut self) {
        let (serving, until, _) = self
            .sent
            .as_mut()
            .expect("a delivered compact announcement retains its serving snapshot");
        serving.authorization = None;
        *until = Some(Instant::now() + RETENTION);
        self.recovery_bytes_left =
            usize::try_from(MAX_BLOCK_BYTES).expect("the block byte limit fits in memory");
    }

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
            .is_some_and(|(_, until, _)| until.is_some_and(|until| until <= now))
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
        if self
            .discovery
            .as_ref()
            .is_some_and(|discovery| discovery.until <= now)
        {
            self.discovery = None;
        }
    }

    /// Consume screened discovery once so later requests can observe the peer's progress.
    pub(super) fn take_discovery(&mut self, tip: &block::Hash) -> Option<Vec<block::Hash>> {
        let discovery = self.discovery.as_ref()?;
        if Instant::now() >= discovery.until || discovery.parent != *tip {
            return None;
        }
        self.discovery.take().map(|discovery| discovery.hashes)
    }

    pub(super) fn is_barrier_nonce(&self, nonce: Nonce) -> bool {
        self.discovery_barrier
            .as_ref()
            .is_some_and(|barrier| barrier.nonce == nonce)
    }

    pub(super) fn barrier_timed_out(&self) -> bool {
        self.discovery_barrier
            .as_ref()
            .is_some_and(|barrier| barrier.deadline <= Instant::now())
    }

    /// Quarantine all block inventories until the owned pong, including unsolicited gossip.
    /// This relies on honest Zebra connections awaiting locator service calls and writes before
    /// handling later pings, including deferred recovery traffic. It is not a service-bit guarantee.
    /// Stock zcashd cannot enter this compact path; a missing owned pong closes it on timeout.
    pub(super) fn divert_retired_discovery(&mut self, message: &Message) -> bool {
        let Some(barrier) = &self.discovery_barrier else {
            return false;
        };
        match message {
            Message::Pong(nonce) if *nonce == barrier.nonce => {
                self.discovery_barrier = None;
                true
            }
            Message::Inv(items) if is_block_inventory(items) => {
                // Only an extension of the screened compact's old locator is reusable. A singleton
                // may be unrelated gossip; it neither consumes the barrier nor repopulates a cache.
                if items.len() > 1 && items[0].block_hash() == Some(barrier.hash) {
                    self.discovery = Some(Discovery {
                        parent: barrier.parent,
                        hashes: block_hashes(items)
                            .take(constants::MAX_FIND_BLOCKS_RESPONSE_HASHES)
                            .collect(),
                        until: Instant::now() + RETENTION,
                    });
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn has_cached(&self) -> bool {
        self.received.is_some()
            || self.sent.is_some()
            || self.reconstructed.is_some()
            || self.discovery.is_some()
            || self.discovery_barrier.is_some()
    }

    pub(super) fn expiry(&self) -> Instant {
        self.received
            .as_ref()
            .map(|(_, until, _)| *until)
            .into_iter()
            .chain(self.sent.as_ref().and_then(|(_, until, _)| *until))
            .chain(self.reconstructed.as_ref().map(|(_, until, _)| *until))
            .chain(self.discovery.as_ref().map(|discovery| discovery.until))
            .chain(
                self.discovery_barrier
                    .as_ref()
                    .map(|barrier| barrier.deadline),
            )
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

    /// Resolve live permission before constructing an announcement.
    async fn compact_serving_block(&mut self, hash: block::Hash) -> Option<ServingBlock> {
        let (blocks, authorization) = match self
            .compact_query(Request::EarlyBlocksByHash(HashSet::from([hash])))
            .await?
        {
            Response::Blocks(blocks) => (blocks, None),
            Response::AuthorizedBlocks {
                blocks,
                authorization,
            } => (blocks, Some(authorization)),
            _ => return None,
        };
        let block = blocks.into_iter().find_map(|item| match item {
            InventoryResponse::Available((block, _)) if block.hash() == hash => Some(block),
            _ => None,
        })?;
        Some(ServingBlock {
            block,
            nonce: rand::random(),
            authorization,
        })
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
        let serving = self.compact_serving_block(hash).await?;
        let (serving, compact, permit) = tokio::task::spawn_blocking(move || {
            let compact = CompactBlock::from_block(&serving.block, serving.nonce);
            let compact = compact.ok().filter(|compact| {
                compact.zcash_serialized_size() < serving.block.zcash_serialized_size()
            });
            (serving, compact, permit)
        })
        .await
        .ok()?;
        let compact = compact?;
        self.compact.sent = Some((serving, None, permit));
        Some(Message::CompactBlock(Arc::new(compact)))
    }

    pub(super) async fn receive_compact(&mut self, block: Arc<CompactBlock>) {
        if !self.connection_info.early_block_relay {
            return;
        }
        self.compact.expire();
        let hash = block.header.hash();
        let advertiser = self.connection_info.connected_addr.get_transient_addr();
        if self.compact.received.is_none()
            && self
                .compact
                .last_received
                .is_none_or(|last| last.elapsed() >= RETENTION)
        {
            // Rejected headers and downloads consume the same cooldown as accepted announcements.
            self.compact.last_received = Some(Instant::now());
            if matches!(
                self.compact_query(Request::CheckCompactHeader(block.clone()))
                    .await,
                Some(Response::Nil)
            ) {
                self.compact.expire();
                let completes_discovery = self.compact.discovery_barrier.is_none()
                    && matches!(
                        self.state,
                        State::AwaitingResponse {
                            handler: Handler::FindBlocks { extends: Some(tip) }, ..
                        } if tip == block.header.previous_block_hash
                    );
                if completes_discovery {
                    let nonce = Nonce::default();
                    if let Err(error) = self.send_download_request(Message::Ping(nonce)).await {
                        self.fail_with(error).await;
                        return;
                    }
                    self.compact.discovery_barrier = Some(DiscoveryBarrier {
                        nonce,
                        parent: block.header.previous_block_hash,
                        hash,
                        deadline: Instant::now() + constants::REQUEST_TIMEOUT,
                    });
                }
                if let Ok(permit) = SLOTS.clone().try_acquire_owned() {
                    if matches!(
                        self.compact_query(Request::AdvertiseCompactBlock(hash, advertiser))
                            .await,
                        Some(Response::Nil)
                    ) {
                        if !completes_discovery {
                            self.compact.discovery = Some(Discovery {
                                parent: block.header.previous_block_hash,
                                hashes: vec![hash],
                                until: Instant::now() + RETENTION,
                            });
                        }
                        self.compact.received = Some((block, Instant::now() + RETENTION, permit));
                    }
                } else {
                    self.drive_peer_request(Request::AdvertiseBlock(hash, advertiser))
                        .await;
                }
                if completes_discovery {
                    // Admission and the framed ping write can both wait. Recheck live eligibility
                    // immediately before returning a synthetic response, like the cached shortcut.
                    if self.connection_info.remote.version < self.minimum_peer_version.current()
                        || advertiser
                            .is_some_and(|addr| self.bans_receiver.borrow().is_banned(addr.ip()))
                    {
                        self.fail_with(PeerError::ConnectionDropped).await;
                        return;
                    }
                    if let State::AwaitingResponse { handler, .. } = &mut self.state {
                        *handler = Handler::Finished(Ok(Response::BlockHashes(vec![hash])));
                    }
                }
                return;
            }
        }
        // Reconstruction starts only after the downloader applies its global/per-IP admission.
        self.drive_peer_request(Request::AdvertiseBlock(hash, advertiser))
            .await;
    }

    pub(super) async fn download_block(&mut self, hash: block::Hash) -> Result<Handler, PeerError> {
        self.compact.expire();
        if let Some((block, _, _)) = &self.compact.reconstructed {
            if block.block().hash() == hash {
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
                        self.send_download_request(Message::GetBlockTransactions(
                            BlockTransactionsRequest {
                                block_hash: hash,
                                indexes,
                            },
                        ))
                        .await?;
                        return Ok(Handler::CompactBlock {
                            reconstruction,
                            permit,
                        });
                    }
                }
            }
        }
        self.send_download_request(Message::GetData(vec![hash.into()]))
            .await?;
        Ok(Handler::BlocksByHash {
            pending_hashes: HashSet::from([hash]),
            blocks: Vec::with_capacity(1),
        })
    }

    /// Recheck peer eligibility immediately before writing a compact recovery or full request.
    async fn send_download_request(&mut self, message: Message) -> Result<(), PeerError> {
        let address = self.connection_info.connected_addr.get_transient_addr();
        let version = self.connection_info.remote.version;
        let minimum = &mut self.minimum_peer_version;
        let bans = &self.bans_receiver;
        self.peer_tx
            .send_authorized(message, || {
                if version < minimum.current()
                    || address.is_some_and(|addr| bans.borrow().is_banned(addr.ip()))
                {
                    Err(PeerError::ConnectionDropped)
                } else {
                    Ok(())
                }
            })
            .await
    }

    fn compact_response(&self, block: ReconstructedBlock) -> Handler {
        Handler::Finished(Ok(Response::ReconstructedBlock(
            block,
            self.connection_info.connected_addr.get_transient_addr(),
        )))
    }

    async fn finish_compact(
        &mut self,
        reconstruction: Reconstruction,
        response: BlockTransactions,
        permit: OwnedSemaphorePermit,
    ) -> Option<ReconstructedBlock> {
        let (block, permit) =
            tokio::task::spawn_blocking(move || (reconstruction.finish(response), permit))
                .await
                .ok()?;
        let block = block.ok()?;
        // A txid Merkle root does not bind v5/v6 proofs or signatures. Authenticate the ordered
        // authorization root before giving these probabilistically reconstructed bytes to consensus.
        if !matches!(
            self.compact_query(Request::CheckCompactBlock(
                block.block().clone(),
                Some(block.clone())
            ))
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
        if let Some(recovery) = &self.compact_recovery {
            recovery.send_replace(None);
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
        self.clear_compact_recovery();
        if let Err(error) = self
            .send_download_request(Message::GetData(vec![hash.into()]))
            .await
        {
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
        if !self.connection_info.early_block_relay {
            return;
        }
        self.compact.expire();
        let Some((serving, Some(until), _)) = &self.compact.sent else {
            return;
        };
        if serving.block.hash() != request.block_hash {
            return;
        }
        if request
            .indexes
            .last()
            .is_some_and(|index| *index >= serving.block.transactions.len())
            || request.indexes.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return;
        }
        let until = *until;
        let overhead = request.block_hash.zcash_serialized_size()
            + CompactSizeMessage::try_from(request.indexes.len())
                .expect("recovery indexes are bounded by the advertised block")
                .zcash_serialized_size();
        let Some(remaining) = self
            .compact
            .recovery_bytes_left
            .checked_sub(overhead)
            .and_then(|remaining| {
                request
                    .indexes
                    .iter()
                    .try_fold(remaining, |remaining, index| {
                        remaining
                            .checked_sub(serving.block.transactions[*index].zcash_serialized_size())
                    })
            })
        else {
            return;
        };
        self.compact.recovery_bytes_left = remaining;
        let transactions = request
            .indexes
            .into_iter()
            .map(|index| serving.block.transactions[index].clone())
            .collect();
        let response = BlockTransactions {
            block_hash: request.block_hash,
            transactions,
        };
        // Repeated recovery shares a byte budget and the snapshot's original deadline.
        let result = timeout_at(
            until,
            self.peer_tx.send(Message::BlockTransactions(response)),
        )
        .await
        .unwrap_or(Err(PeerError::ConnectionSendTimeout));
        if let Err(error) = result {
            self.compact.clear_sent();
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
