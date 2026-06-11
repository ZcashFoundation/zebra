//! The batched block fetch service for the known-hash IBD engine.
//!
//! Batching is a [`tower_batch_control`] layer over the [`FetchBatcher`] inner
//! service: the engine issues one weighted [`FetchRequest`] per block, the
//! batch worker groups them by size-hint weight, and each request gets its own
//! response future, so per-block response distribution is native.
//!
//! On every flush, the batcher issues **one** [`zn::Request::BlocksByHash`]
//! through the existing Buffer'd peer set, matches `Available` blocks back to
//! items by header hash, and classifies failures per
//! `docs/design/known-hash-ibd.md` §4.2:
//!
//! - explicit peer `notfound` ⇒ [`FetchError::NotFound`] (per-height peer
//!   exclusion in the engine, after one immediate in-batcher retry round for
//!   the missing remainder), and
//! - whole-request errors and timeouts ⇒ [`FetchError::Transport`] for every
//!   item in the batch, with **no** peer exclusion.
//!
//! The engine never writes to the peer set's inventory registry: marking stays
//! in the client layer (`MissingInventoryCollector`), which only forwards
//! explicit `notfound`s, so transport errors never poison routing
//! (the #10679/#5709 contract).

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::FutureExt;
use thiserror::Error;
use tokio::sync::oneshot;
use tower::{Service, ServiceExt};
use tower_batch_control::{Batch, BatchControl, RequestWeight};

use zebra_chain::{
    block::{self, Block},
    parameters::known_hashes::SIZE_HINT_UNIT,
};
use zebra_network::{self as zn, InventoryResponse, PeerSocketAddr};

use super::engine::{IBD_BATCH_MAX_BLOCKS, IBD_BATCH_MAX_WEIGHT};
use crate::BoxError;

/// The size hint used for blocks whose known-hash chunk does not embed
/// per-block size hints.
///
/// `255` is the maximum hint, covering the largest possible block, so the
/// byte-budget reservations stay a priori upper bounds — but it also makes
/// every request cross the batch weight threshold on its own, so batches are
/// effectively single-block. With hinted chunks, small-block eras batch up
/// to [`IBD_BATCH_MAX_BLOCKS`] blocks per request.
pub const DEFAULT_SIZE_HINT: u8 = 255;

/// The minimum [`RequestWeight`] of a fetch item.
///
/// `tower-batch-control` has a single flush-after-crossing weight threshold
/// and no separate item-count limit, so the 16-block wire serving limit
/// ([`IBD_BATCH_MAX_BLOCKS`]) is encoded into the weights instead: with every
/// item weighing at least `IBD_BATCH_MAX_WEIGHT / IBD_BATCH_MAX_BLOCKS`, the
/// 16th item always crosses the threshold and flushes the batch.
///
/// Inflating a tiny block's weight to the floor can only make batches
/// *smaller* than the byte threshold alone would, so the §4.2 serving
/// analysis (all-but-the-last item sum to under the threshold) still holds
/// for the real serialized bytes.
const ITEM_WEIGHT_FLOOR: usize = IBD_BATCH_MAX_WEIGHT.div_ceil(IBD_BATCH_MAX_BLOCKS);

// A weight floor below `hint = 1`'s byte bound would allow more than 16
// items per batch; a floor above the threshold would force single-item
// batches even with real hints.
const _: () = assert!(ITEM_WEIGHT_FLOOR * IBD_BATCH_MAX_BLOCKS >= IBD_BATCH_MAX_WEIGHT);
const _: () = assert!(ITEM_WEIGHT_FLOOR < IBD_BATCH_MAX_WEIGHT);

/// A request to fetch one block by its pinned known-hash list entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    /// The block's height, for diagnostics; the wire request is by hash.
    pub height: block::Height,

    /// The block's pinned hash from the known-hash list.
    pub hash: block::Hash,

    /// The block's size hint from the known-hash list: serialized size is at
    /// most `size_hint × SIZE_HINT_UNIT` bytes (design doc §6.2).
    ///
    /// Always in `1..=255`; until the size-hint asset ships this is
    /// [`DEFAULT_SIZE_HINT`].
    pub size_hint: u8,
}

impl FetchRequest {
    /// The a priori upper bound on this block's serialized size, in bytes.
    ///
    /// Hints round sizes up, so budgets reserved from this bound can only
    /// over-reserve, never under-count.
    pub fn hint_upper_bytes(&self) -> u64 {
        // a u8 hint times the ~8 KB unit always fits in u64
        u64::from(self.size_hint.max(1)) * u64::from(SIZE_HINT_UNIT)
    }
}

impl RequestWeight for FetchRequest {
    /// The size-hint upper bound in bytes, floored so that
    /// [`IBD_BATCH_MAX_BLOCKS`] items always cross the batch weight threshold
    /// (see [`ITEM_WEIGHT_FLOOR`]).
    fn request_weight(&self) -> usize {
        // the hint upper bound is at most 255 × SIZE_HINT_UNIT ≈ 2 MB,
        // which fits in usize on all supported platforms
        usize::try_from(self.hint_upper_bytes())
            .expect("size-hint upper bounds are far below usize::MAX")
            .max(ITEM_WEIGHT_FLOOR)
    }
}

/// A successfully fetched block and the peer that delivered it.
#[derive(Clone, Debug)]
pub struct FetchedBlock {
    /// The block, already checked against the requested hash.
    pub block: Arc<Block>,

    /// The address of the peer that delivered the block, when known; used for
    /// misbehavior attribution.
    pub source: Option<PeerSocketAddr>,
}

/// The response type shared by per-item and flush calls on the
/// [`FetchBatcher`].
///
/// `tower-batch-control` requires one response type for both call kinds; the
/// engine only ever receives [`Self::Fetched`], because [`Self::Flushed`] is
/// resolved by the batch worker's internal flush future.
#[derive(Clone, Debug)]
pub enum BatchFetchResponse {
    /// A per-item response: the requested block arrived.
    Fetched(FetchedBlock),

    /// A flush completed; only seen by the batch worker.
    Flushed,
}

/// A classified block fetch failure (design doc §4.2).
///
/// The classification mirrors the inventory-registry taxonomy from
/// #10679/#5709: only explicit peer `notfound` responses mean "this peer
/// lacks the block"; timeouts, drops, and overload mean nothing about the
/// peer's inventory.
#[derive(Clone, Debug, Error)]
pub enum FetchError {
    /// A peer explicitly reported the block as `notfound`, and the immediate
    /// in-batcher retry round did not recover it.
    ///
    /// The engine excludes `peer` for this height (when the responding peer
    /// is known) and re-issues the fetch.
    #[error("block {hash:?} at {height:?} not found by peer {peer:?}")]
    NotFound {
        /// The requested block hash.
        hash: block::Hash,
        /// The requested block height.
        height: block::Height,
        /// The peer that reported `notfound`, when identifiable from the
        /// same response's `Available` entries.
        peer: Option<PeerSocketAddr>,
    },

    /// The whole batch request failed in transport: timeout, connection
    /// drop, overload, or an unexpected response shape.
    ///
    /// Carries no information about any peer's inventory, so the engine
    /// re-issues without excluding anyone.
    #[error("transport error fetching block batch: {0}")]
    Transport(String),
}

impl FetchError {
    /// The engine-facing classification of this error.
    pub fn kind(&self) -> FetchFailureKind {
        match self {
            FetchError::NotFound { peer, .. } => FetchFailureKind::NotFound { peer: *peer },
            FetchError::Transport(_) => FetchFailureKind::Transport,
        }
    }

    /// Classifies a boxed error from the batched fetch service.
    ///
    /// Errors that are not a [`FetchError`] (batch worker shutdown, closed
    /// channels) are conservatively classified as transport losses: they say
    /// nothing about peer inventory.
    pub fn classify(error: &BoxError) -> FetchFailureKind {
        error
            .downcast_ref::<FetchError>()
            .map_or(FetchFailureKind::Transport, FetchError::kind)
    }
}

/// The engine-facing classification of a fetch failure.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FetchFailureKind {
    /// Explicit peer `notfound`: exclude `peer` for this height and re-issue.
    NotFound {
        /// The peer that reported `notfound`, when known.
        peer: Option<PeerSocketAddr>,
    },

    /// Transport loss: re-issue with no peer exclusion.
    Transport,
}

/// The type of the per-item response channel.
type ItemResponder = oneshot::Sender<Result<FetchedBlock, FetchError>>;

/// The batched fetch service exposed to the engine.
pub type BatchedFetch<ZN> = Batch<FetchBatcher<ZN>, FetchRequest>;

/// Returns the engine's batched fetch service: a [`tower_batch_control`]
/// layer over a [`FetchBatcher`] on the Buffer'd peer set.
///
/// `max_concurrent_batches` comes from
/// [`ibd_max_concurrent_batches`](super::engine::ibd_max_concurrent_batches);
/// the weight threshold and flush latency are the §3.2 constants.
///
/// Must be called within a Tokio runtime: the batch worker is spawned onto
/// the runtime.
pub fn batched_fetch<ZN>(network: ZN, max_concurrent_batches: usize) -> BatchedFetch<ZN>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    Batch::new(
        FetchBatcher::new(network),
        IBD_BATCH_MAX_WEIGHT,
        max_concurrent_batches,
        super::engine::IBD_BATCH_FLUSH_LATENCY,
    )
}

/// The inner `Service<BatchControl<FetchRequest>>` driven by the
/// [`tower_batch_control`] worker.
///
/// `Item` calls stash the request and its responder and return a future that
/// resolves when the batch round fulfills it; `Flush` calls take the pending
/// items and resolve one network round for all of them.
pub struct FetchBatcher<ZN>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    /// The Buffer'd peer set; cloned into each flushed batch round.
    network: ZN,

    /// The items accumulated since the last flush, with their responders.
    pending: Vec<(FetchRequest, ItemResponder)>,
}

impl<ZN> FetchBatcher<ZN>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    /// Returns a new batcher over the Buffer'd peer set.
    pub fn new(network: ZN) -> Self {
        Self {
            network,
            pending: Vec::new(),
        }
    }
}

impl<ZN> Service<BatchControl<FetchRequest>> for FetchBatcher<ZN>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    type Response = BatchFetchResponse;
    type Error = FetchError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Always ready: `call` only stashes items or moves them into a flush
        // future, and the flush future waits for the Buffer'd peer set's own
        // readiness. This also satisfies the `tower-batch-control` worker's
        // correctness requirement that the inner service never returns
        // recoverable errors from `poll_ready`.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, control: BatchControl<FetchRequest>) -> Self::Future {
        match control {
            BatchControl::Item(request) => {
                let (responder, response) = oneshot::channel();
                self.pending.push((request, responder));

                async move {
                    match response.await {
                        Ok(Ok(fetched)) => Ok(BatchFetchResponse::Fetched(fetched)),
                        Ok(Err(error)) => Err(error),
                        // The flush future was dropped before responding
                        // (worker shutdown): nothing is known about the peer.
                        Err(_recv_error) => Err(FetchError::Transport(
                            "batch dropped before the block arrived".into(),
                        )),
                    }
                }
                .boxed()
            }

            BatchControl::Flush => {
                let items = std::mem::take(&mut self.pending);
                let network = self.network.clone();

                async move {
                    if !items.is_empty() {
                        metrics::gauge!("ibd.inflight.batches").increment(1.0);
                        fetch_batch(network, items).await;
                        metrics::gauge!("ibd.inflight.batches").decrement(1.0);
                    }

                    // Always `Ok`: a flush error would permanently fail the
                    // batch worker (see `Batch::poll_ready`'s correctness
                    // note); per-item failures went through the responders.
                    Ok(BatchFetchResponse::Flushed)
                }
                .boxed()
            }
        }
    }
}

/// Resolves one flushed batch: issues `BlocksByHash` rounds and fulfills
/// every item's responder with a block or a classified error.
///
/// The weight floor normally caps batches at [`IBD_BATCH_MAX_BLOCKS`] items;
/// chunking here makes the wire limit a hard guarantee even if weights
/// misbehave, because overflowing a `getdata` is silently truncated by
/// serving nodes.
async fn fetch_batch<ZN>(network: ZN, items: Vec<(FetchRequest, ItemResponder)>)
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    let mut items = items;

    while !items.is_empty() {
        let rest = items.split_off(items.len().min(IBD_BATCH_MAX_BLOCKS));
        fetch_chunk(network.clone(), items).await;
        items = rest;
    }
}

/// Resolves up to [`IBD_BATCH_MAX_BLOCKS`] items with one `BlocksByHash`
/// request, plus one immediate retry round for an explicit-`notfound`
/// remainder (design doc §4.2).
async fn fetch_chunk<ZN>(network: ZN, items: Vec<(FetchRequest, ItemResponder)>)
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    /// One initial round and one retry round for explicit-notfound partials.
    const ROUNDS: usize = 2;

    let mut remaining: HashMap<block::Hash, (FetchRequest, ItemResponder)> = items
        .into_iter()
        .map(|(request, responder)| (request.hash, (request, responder)))
        .collect();

    // The peer that most recently reported `notfound`, when identifiable
    // from the same response's `Available` entries (a batch is served by a
    // single peer, but `Missing` entries don't carry the address).
    let mut not_found_peer: Option<PeerSocketAddr> = None;

    for round in 0..ROUNDS {
        let hashes = remaining.keys().copied().collect();

        let response = network
            .clone()
            .oneshot(zn::Request::BlocksByHash(hashes))
            .await;

        let response_items = match response {
            Ok(zn::Response::Blocks(response_items)) => response_items,
            Ok(unexpected) => {
                // A wrong response variant says nothing about peer
                // inventory: classify as transport for all remaining items.
                fail_all(remaining, |_| {
                    FetchError::Transport(format!("unexpected response: {unexpected}"))
                });
                return;
            }
            Err(error) => {
                // Whole-request error or timeout: the connection handler
                // discarded any accumulated blocks, so every item is a
                // transport loss and no peer is excluded.
                fail_all(remaining, |_| FetchError::Transport(error.to_string()));
                return;
            }
        };

        let mut saw_missing = false;
        let mut round_source: Option<PeerSocketAddr> = None;

        for response_item in response_items {
            match response_item {
                InventoryResponse::Available((block, source)) => {
                    // `Available` entries carry no request hash, so matching
                    // recomputes the header hash: one ~µs hash per block.
                    let hash = block.hash();

                    round_source = round_source.or(source);

                    // Unsolicited and duplicate blocks fall through and are
                    // dropped here; the connection handler already filters
                    // blocks that match no requested hash.
                    if let Some((_request, responder)) = remaining.remove(&hash) {
                        let _ = responder.send(Ok(FetchedBlock { block, source }));
                    }
                }
                InventoryResponse::Missing(_hash) => {
                    saw_missing = true;
                }
            }
        }

        if saw_missing {
            // A batch is served by one peer, so this round's `Available`
            // source is also whoever reported the `Missing` items. Attribute
            // to the most recent round that saw a miss, not the earliest:
            // keeping a stale earlier peer (via `.or`) would blame a peer that
            // delivered in a later round, and overwriting with `None` for an
            // all-missing round honestly records "couldn't identify the peer".
            not_found_peer = round_source;
        }

        if remaining.is_empty() {
            return;
        }

        if !saw_missing {
            // Items absent without an explicit `notfound` (a malformed
            // partial response): transport loss, no exclusion.
            fail_all(remaining, |_| {
                FetchError::Transport("response did not cover all requested blocks".into())
            });
            return;
        }

        if round == ROUNDS - 1 {
            // The retry round was also an explicit notfound: classify so the
            // engine can exclude the reporting peer and re-issue.
            fail_all(remaining, |request| FetchError::NotFound {
                hash: request.hash,
                height: request.height,
                peer: not_found_peer,
            });
            return;
        }
    }
}

/// Fails every remaining item with the error produced by `error_for`.
fn fail_all(
    remaining: HashMap<block::Hash, (FetchRequest, ItemResponder)>,
    error_for: impl Fn(&FetchRequest) -> FetchError,
) {
    for (_hash, (request, responder)) in remaining {
        let _ = responder.send(Err(error_for(&request)));
    }
}

/// Fetches a single block by hash directly through the peer set, bypassing
/// the batch layer.
///
/// This is the engine's gap-hedge path: a single-hash `BlocksByHash` gets the
/// peer set's inventory-aware routing (`route_inv`) for free — advertising
/// peers preferred, known-missing peers skipped — and the
/// one-request-per-connection rule lands the hedge on a different peer than
/// the still-pending primary by construction. No internal retry round: the
/// engine's retry policy owns hedge failures.
pub(crate) async fn fetch_single<ZN>(
    network: ZN,
    height: block::Height,
    hash: block::Hash,
) -> Result<FetchedBlock, FetchError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    let response = network
        .oneshot(zn::Request::BlocksByHash(std::iter::once(hash).collect()))
        .await;

    let response_items = match response {
        Ok(zn::Response::Blocks(response_items)) => response_items,
        Ok(unexpected) => {
            return Err(FetchError::Transport(format!(
                "unexpected response: {unexpected}"
            )));
        }
        Err(error) => return Err(FetchError::Transport(error.to_string())),
    };

    for response_item in response_items {
        match response_item {
            InventoryResponse::Available((block, source)) if block.hash() == hash => {
                return Ok(FetchedBlock { block, source });
            }
            // Unsolicited blocks for other hashes are dropped.
            InventoryResponse::Available(_) => {}
            InventoryResponse::Missing(_) => {
                return Err(FetchError::NotFound {
                    hash,
                    height,
                    peer: None,
                });
            }
        }
    }

    Err(FetchError::Transport(
        "response did not contain the requested block".into(),
    ))
}
