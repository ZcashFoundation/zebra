//! Opt-in compact block transport for committed blocks.
//!
//! A negotiated peer announces a new block as its header, short transaction IDs and coinbase.
//! The receiver rebuilds the block from its mempool, asks once for the transactions it is missing,
//! and falls back to an ordinary `getdata` on any problem. Consensus checks and source scoring
//! remain in the ordinary downloader.

use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, LazyLock},
    time::Duration,
};

use futures::Sink;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{sleep, sleep_until, timeout_at, Instant, Sleep},
};
use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{self, Block, MAX_BLOCK_BYTES},
    chain_tip::ChainTip,
    serialization::{CompactSizeMessage, FakeWriter, SerializationError, ZcashSerialize},
};

use super::{Connection, Handler, InventoryResponse, State};
use crate::{
    compact::{BlockTransactions, BlockTransactionsRequest, CompactBlock, Reconstruction},
    constants,
    peer_set::InventoryChange,
    protocol::external::{InventoryHash, Message},
    BoxError, PeerError, Request, Response,
};

// A decoded body can retain substantially more memory than its two-megabyte wire encoding.
// Global slots bound the bodies kept across all connections, including detached hash work.
// Without a free slot, blocks are announced or downloaded in full.
//
// Announced blocks and peers' bodies have separate slots, so connections that hold the
// announcements of this node's blocks can't stop it receiving compact blocks, or the other way
// around.
//
// Announced blocks are committed blocks, which the state also holds.
pub(super) static SENT_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(6)));
// Received bodies must pass proof-of-work and current-best-tip screening, and then win the block
// downloader's admission for their hash, before they keep a slot. A rebuilt block keeps the slot
// of the body it was built from. A connection keeps at most one received body and one rebuilt
// block, and one announced block.
pub(super) static RECEIVED_SLOTS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(6)));
const RETENTION: Duration = Duration::from_secs(15);
/// The number of blocks a connection remembers its peer announcing in compact form.
const ANNOUNCED_BLOCKS: usize = 4;
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);

struct ServingBlock {
    block: Arc<Block>,
    /// Keep the sent short-ID keys reproducible without retaining the compact transaction vectors.
    nonce: u64,
}

#[derive(Default)]
pub(super) struct CompactRelay {
    pub(super) send: bool,
    received: Option<(Arc<CompactBlock>, Instant, OwnedSemaphorePermit)>,
    sent: Option<(ServingBlock, Option<Instant>, OwnedSemaphorePermit)>,
    /// Bound aggregate recovery payloads, including repeated or empty requests.
    recovery_bytes_left: usize,
    /// Gossip and sync can request the same body before either verifier commits it.
    reconstructed: Option<(Arc<Block>, Instant, OwnedSemaphorePermit)>,
    last_received: Option<Instant>,
    /// The deadline of the block download that this connection has just started, for the
    /// request timer.
    download_deadline: Option<Instant>,
    /// The blocks that the peer announced in compact form most recently, whether or not this
    /// connection kept them.
    announced: VecDeque<block::Hash>,
}

impl CompactRelay {
    pub(super) fn clear_sent(&mut self) {
        self.sent = None;
    }

    /// Drops every body this connection keeps, which releases their global slots.
    pub(super) fn release(&mut self) {
        self.received = None;
        self.sent = None;
        self.reconstructed = None;
    }

    /// Returns the deadline of the block download that this connection has just started.
    pub(super) fn take_download_deadline(&mut self) -> Option<Instant> {
        self.download_deadline.take()
    }

    /// Returns true if this peer's compact announcement can answer a download of `hash`.
    pub(super) fn is_cached(&self, hash: block::Hash) -> bool {
        self.received
            .as_ref()
            .is_some_and(|(block, _, _)| block.header.hash() == hash)
            || self
                .reconstructed
                .as_ref()
                .is_some_and(|(block, _, _)| block.hash() == hash)
    }

    /// Records that the peer announced the block `hash` in compact form.
    fn note_announced(&mut self, hash: block::Hash) {
        if self.announced.contains(&hash) {
            return;
        }
        if self.announced.len() == ANNOUNCED_BLOCKS {
            self.announced.pop_front();
        }
        self.announced.push_back(hash);
    }

    /// Returns true if the peer announced the block `hash` in compact form recently, so it
    /// already has that block.
    fn has_announced(&self, hash: block::Hash) -> bool {
        self.announced.contains(&hash) || self.is_cached(hash)
    }

    pub(super) fn mark_sent(&mut self) {
        let (_, until, _) = self
            .sent
            .as_mut()
            .expect("a delivered compact announcement retains its serving snapshot");
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
    }

    pub(super) fn has_cached(&self) -> bool {
        self.received.is_some() || self.sent.is_some() || self.reconstructed.is_some()
    }

    pub(super) fn expiry(&self) -> Instant {
        self.received
            .as_ref()
            .map(|(_, until, _)| *until)
            .into_iter()
            .chain(self.sent.as_ref().and_then(|(_, until, _)| *until))
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
        self.compact_query_until(request, Instant::now() + constants::REQUEST_TIMEOUT)
            .await
    }

    /// Like [`Self::compact_query`], but gives up at `deadline`.
    async fn compact_query_until(
        &mut self,
        request: Request,
        deadline: Instant,
    ) -> Option<Response> {
        timeout_at(deadline, async {
            self.svc.ready().await.ok()?.call(request).await.ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Recheck this peer's authorization after sink readiness, as ordinary requests do.
    ///
    /// The write stops at `deadline`, the deadline of the block download that it is part of.
    async fn send_compact_request(
        &mut self,
        message: Message,
        deadline: Instant,
    ) -> Result<(), PeerError> {
        let address = self.connection_info.connected_addr.get_transient_addr();
        let version = self.connection_info.remote.version;
        let minimum = &mut self.minimum_peer_version;
        let bans = &self.bans_receiver;
        let send = self.peer_tx.send_authorized(message, || {
            if version < minimum.current()
                || address.is_some_and(|addr| bans.borrow().is_banned(addr.ip()))
            {
                Err(PeerError::ConnectionDropped)
            } else {
                Ok(())
            }
        });

        // A write that is cut off can leave part of a message in the sink. So this fails the
        // connection, like the usual send timeout does.
        timeout_at(deadline, send)
            .await
            .map_err(|_| PeerError::ConnectionSendTimeout)?
    }

    /// Fetch the committed block before constructing an announcement.
    async fn compact_serving_block(&mut self, hash: block::Hash) -> Option<ServingBlock> {
        let Response::Blocks(blocks) = self
            .compact_query(Request::BlocksByHash(HashSet::from([hash])))
            .await?
        else {
            return None;
        };
        let block = blocks.into_iter().find_map(|item| match item {
            InventoryResponse::Available((block, _)) if block.hash() == hash => Some(block),
            _ => None,
        })?;
        Some(ServingBlock {
            block,
            nonce: rand::random(),
        })
    }

    pub(super) async fn compact_advert(&mut self, hash: block::Hash) -> Option<Message> {
        if !self.connection_info.early_block_relay || !self.compact.send {
            return None;
        }
        self.compact.expire();
        // The peer announced this block to this node, so it gets the block inventory, as it would
        // without compact relay.
        if self.compact.has_announced(hash) {
            return None;
        }
        // Do not replace a body while the peer can still be recovering its transactions.
        if self.compact.sent.is_some() {
            return None;
        }
        let permit = SENT_SLOTS.clone().try_acquire_owned().ok()?;
        let serving = self.compact_serving_block(hash).await?;
        let (serving, compact, permit) = tokio::task::spawn_blocking(move || {
            let compact = CompactBlock::from_block(&serving.block, serving.nonce)
                .ok()
                .and_then(|compact| smaller_announcement(compact, &serving.block));
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
        self.compact.note_announced(hash);
        let advertiser = self.connection_info.connected_addr.get_transient_addr();
        if self.compact.received.is_none()
            && self
                .compact
                .last_received
                .is_none_or(|last| last.elapsed() >= RETENTION)
            && matches!(
                self.compact_query(Request::CheckCompactHeader(block.clone()))
                    .await,
                Some(Response::Nil)
            )
        {
            if let Ok(permit) = RECEIVED_SLOTS.clone().try_acquire_owned() {
                // The peer has this screened block, even if the downloader takes it from another
                // peer, or doesn't take it now.
                self.register_compact_announcement(hash);
                if matches!(
                    self.compact_query(Request::AdvertiseCompactBlock(hash, advertiser))
                        .await,
                    Some(Response::Nil)
                ) {
                    metrics::counter!("zebra.net.compact_block.admitted").increment(1);
                    self.compact.last_received = Some(Instant::now());
                    self.compact.received = Some((block, Instant::now() + RETENTION, permit));
                }
                return;
            }
        }
        // Reconstruction starts only after the downloader applies its global/per-IP admission.
        self.register_compact_announcement(hash);
        self.drive_peer_request(Request::AdvertiseBlock(hash, advertiser))
            .await;
    }

    /// Tells the peer set that this peer has the block it announced, like a block inventory does,
    /// so downloads of the block without a source can be sent to this peer.
    ///
    /// A compact announcement replaces the block inventory, and this is only called once the
    /// announcement is screened, or handled like an inventory.
    fn register_compact_announcement(&self, hash: block::Hash) {
        if let (Some(inventory), Some(advertiser)) = (
            &self.compact_inventory,
            self.connection_info.connected_addr.get_transient_addr(),
        ) {
            let _ = inventory.send(InventoryChange::new_available(
                InventoryHash::Block(hash),
                advertiser,
            ));
        }
    }

    pub(super) async fn download_block(&mut self, hash: block::Hash) -> Result<Handler, PeerError> {
        // The whole download, including a fallback to the full block, has one request timeout.
        let deadline = Instant::now() + constants::REQUEST_TIMEOUT;
        self.compact.download_deadline = Some(deadline);
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
                .compact_query_until(Request::CompactBlockTransactions(compact.clone()), deadline)
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
                                deadline,
                            )
                            .await
                        {
                            return Ok(self.compact_response(block));
                        }
                    } else if indexes.len() < reconstruction.transaction_count().div_ceil(2) {
                        // Recovering most of a block costs about as much as downloading it, and
                        // a recovery that misses its short timeout is downloaded again in full.
                        // So when at least half the transactions are missing, this downloads
                        // the full block straight away.
                        self.send_compact_request(
                            Message::GetBlockTransactions(BlockTransactionsRequest {
                                block_hash: hash,
                                indexes,
                            }),
                            deadline,
                        )
                        .await?;
                        return Ok(Handler::CompactBlock {
                            reconstruction,
                            permit,
                            deadline,
                        });
                    }
                }
            }
        }
        self.send_compact_request(Message::GetData(vec![hash.into()]), deadline)
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
        deadline: Instant,
    ) -> Option<Arc<Block>> {
        let (block, permit) =
            tokio::task::spawn_blocking(move || (reconstruction.finish(response), permit))
                .await
                .ok()?;
        let block = block.ok()?;
        // A txid Merkle root does not bind v5/v6 proofs or signatures. Authenticate the ordered
        // authorization root before giving these probabilistically reconstructed bytes to consensus.
        if !matches!(
            self.compact_query_until(Request::CheckCompactBlock(block.clone()), deadline)
                .await,
            Some(Response::Nil)
        ) {
            return None;
        }
        metrics::counter!("zebra.net.compact_block.rebuilt").increment(1);
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
            deadline,
        } = std::mem::replace(handler, Handler::Finished(Ok(Response::Nil)))
        else {
            unreachable!("the matching reconstruction was checked")
        };
        if let Some(block) = self
            .finish_compact(reconstruction, response, permit, deadline)
            .await
        {
            let response = self.compact_response(block);
            if let State::AwaitingResponse { handler, .. } = &mut self.state {
                *handler = response;
            }
        } else {
            self.fallback_compact(hash, deadline).await;
        }
    }

    /// Downloads the full block, by the deadline of the compact download it replaces.
    pub(super) async fn fallback_compact(&mut self, hash: block::Hash, deadline: Instant) {
        self.clear_compact_recovery();
        if let Err(error) = self
            .send_compact_request(Message::GetData(vec![hash.into()]), deadline)
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
            self.request_timer = Some(Box::pin(sleep_until(deadline)));
        }
    }

    pub(super) async fn serve_compact_transactions(&mut self, request: BlockTransactionsRequest) {
        if !self.connection_info.early_block_relay {
            return;
        }
        self.compact.expire();
        let Some((serving, Some(_), _)) = &self.compact.sent else {
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
        // Repeated recovery shares a byte budget, and requests are only answered until the snapshot
        // expires. The reply gets the usual send timeout, because an honest peer can ask late.
        if let Err(error) = self
            .peer_tx
            .send(Message::BlockTransactions(response))
            .await
        {
            self.compact.clear_sent();
            self.fail_with(error).await;
        }
    }
}

/// Returns `compact` if its encoding is smaller than `block`'s encoding.
///
/// An announcement that wouldn't save bytes, or whose encoding fails, isn't sent, so the block is
/// relayed with an ordinary block inventory instead.
pub(super) fn smaller_announcement(compact: CompactBlock, block: &Block) -> Option<CompactBlock> {
    (encoded_size(&compact)? < encoded_size(block)?).then_some(compact)
}

/// Returns the number of bytes in `value`'s encoding, or `None` if its encoding fails.
fn encoded_size(value: &impl ZcashSerialize) -> Option<usize> {
    let mut writer = FakeWriter(0);
    value.zcash_serialize(&mut writer).ok()?;
    Some(writer.0)
}

/// Returns the timer for the response to `handler`.
///
/// A block download that started through compact relay keeps the deadline it started with, so the
/// whole download, including a fallback to the full block, has one request timeout.
pub(super) fn response_timer(handler: &Handler, download_deadline: Option<Instant>) -> Sleep {
    match (handler, download_deadline) {
        (Handler::CompactBlock { deadline, .. }, _) => {
            sleep_until((Instant::now() + RECOVERY_TIMEOUT).min(*deadline))
        }
        (_, Some(deadline)) => sleep_until(deadline),
        (_, None) => sleep(constants::REQUEST_TIMEOUT),
    }
}
