//! Mempool-owned mining templates and per-request payout overrides.
//!
//! All inputs come from verified storage snapshots. The retained build and look-ahead coinbase
//! finish even when their parent changes, so tip churn cannot detach CPU-heavy proof work.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout, Instant, Sleep},
};
use tower::{buffer::Buffer, util::BoxService, ServiceExt};

use zebra_chain::{
    amount::{Amount, NegativeOrZero, NonNegative},
    block::{self, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, ZcashSerialize},
    transaction::VerifiedUnminedTx,
};
use zebra_node_services::mempool::TransactionDependencies;
use zebra_rpc::{
    fetch_chain_info, proposal_block_from_template, select_mempool_transactions,
    BlockTemplateRequest, BlockTemplateResponse, CoinbaseCache, LongPollInput, MinerParams,
    TransactionTemplate, MEMPOOL_LONG_POLL_INTERVAL,
};
use zebra_state::{ReadRequest, ReadResponse};

use crate::{components::sync::BLOCK_VERIFY_TIMEOUT, BoxError};

use super::storage::Storage;

#[cfg(test)]
mod tests;

pub(super) type ReadState = Buffer<BoxService<ReadRequest, ReadResponse, BoxError>, ReadRequest>;
pub(super) type BlockVerifier =
    Buffer<BoxService<zebra_consensus::Request, block::Hash, BoxError>, zebra_consensus::Request>;

type CoinbaseTask = (Height, JoinHandle<TransactionTemplate<NegativeOrZero>>);
type Snapshot = (block::Hash, Vec<VerifiedUnminedTx>, TransactionDependencies);

/// Delay after a transient state/consensus error or a superseded parent.
const RETRY_DELAY: Duration = Duration::from_secs(1);

struct Build {
    future: BoxFuture<
        'static,
        (
            Result<Option<BlockTemplateResponse>, BoxError>,
            Option<CoinbaseTask>,
        ),
    >,
    request: Option<BlockTemplateRequest>,
    coinbase_only: bool,
}

/// The only owner of template publication and its bounded proof work.
pub(super) struct BlockTemplates {
    network: Network,
    miner_params: Option<MinerParams>,
    read_state: ReadState,
    verifier: BlockVerifier,
    coinbase_cache: CoinbaseCache,
    published: watch::Sender<Option<Arc<BlockTemplateResponse>>>,
    requests: mpsc::Receiver<BlockTemplateRequest>,
    build: Option<Build>,
    next_coinbase: Option<CoinbaseTask>,
    refresh: Pin<Box<Sleep>>,
    dirty: bool,
    was_failing: bool,
}

impl BlockTemplates {
    pub(super) fn new(
        network: Network,
        miner_params: Option<MinerParams>,
        read_state: ReadState,
        verifier: BlockVerifier,
    ) -> (
        Self,
        watch::Receiver<Option<Arc<BlockTemplateResponse>>>,
        mpsc::Sender<BlockTemplateRequest>,
    ) {
        let (published, templates) = watch::channel(None);
        let (request_sender, requests) = mpsc::channel(1);
        (
            Self {
                network,
                miner_params,
                read_state,
                verifier,
                coinbase_cache: CoinbaseCache::default(),
                published,
                requests,
                build: None,
                next_coinbase: None,
                refresh: Box::pin(sleep(Duration::ZERO)),
                dirty: true,
                was_failing: false,
            },
            templates,
            request_sender,
        )
    }

    pub(super) fn mark_changed(&mut self) {
        self.dirty = true;
    }

    /// Poll owned work without withholding readiness from other mempool requests.
    pub(super) fn poll(
        &mut self,
        cx: &mut Context<'_>,
        storage: Option<(&Storage, block::Hash)>,
        tip: Option<block::Hash>,
    ) -> Poll<Result<(), BoxError>> {
        let mut retry_request = None;
        if let Some(build) = &mut self.build {
            let Poll::Ready((result, next_coinbase)) = build.future.poll_unpin(cx) else {
                return Poll::Ready(Ok(()));
            };
            let build = self
                .build
                .take()
                .expect("the completed build is still owned");
            self.next_coinbase = next_coinbase;

            if result
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .is_some_and(|template| {
                    !template.is_valid_for_tip(
                        template.previous_block_hash(),
                        &self.network,
                        DateTime32::now(),
                    )
                })
            {
                // Testnet's abbreviated standard-difficulty range expired during verification.
                // Retain proof ownership and refetch chain info before publishing any work.
                retry_request = build.request;
                if retry_request.is_none() {
                    self.refresh.as_mut().reset(Instant::now());
                }
            } else if let Some(request) = build.request {
                let result = result.and_then(|template| {
                    template.ok_or_else(|| "chain tip changed while building the template".into())
                });
                let _ = request.response.send(result);
            } else {
                match result {
                    Ok(Some(template)) => {
                        if self.was_failing {
                            tracing::info!("block template builds recovered");
                            self.was_failing = false;
                        }
                        let next_height = Height(template.height()).next().ok();
                        let now = DateTime32::now();
                        let refresh_after = if template.max_time() >= now {
                            (template.max_time().saturating_duration_since(now).to_std()
                                + Duration::from_secs(1))
                            .min(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL))
                        } else {
                            Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)
                        };
                        self.published.send_replace(Some(Arc::new(template)));
                        self.refresh.as_mut().reset(Instant::now() + refresh_after);
                        if build.coinbase_only && self.dirty {
                            // Publish new-tip coinbase work first, then fill it once. Subsequent
                            // same-tip changes wait for the fixed refresh deadline.
                            self.refresh.as_mut().reset(Instant::now());
                        }
                        if let (Some(height), Some(miner_params)) =
                            (next_height, &self.miner_params)
                        {
                            start_precomputing_coinbase(
                                &mut self.next_coinbase,
                                &self.network,
                                miner_params,
                                height,
                            );
                        }
                    }
                    result => {
                        if let Err(error) = result {
                            if self.was_failing {
                                tracing::debug!(?error, "failed to build a block template");
                            } else {
                                tracing::warn!(
                                    ?error,
                                    "failed to build a block template; retaining the last valid work"
                                );
                                self.was_failing = true;
                            }
                        }
                        self.dirty = false;
                        self.refresh.as_mut().reset(Instant::now() + RETRY_DELAY);
                    }
                }
            }
        }

        let needs_tip =
            self.published.borrow().as_ref().is_none_or(|template| {
                tip.is_some_and(|tip| template.previous_block_hash() != tip)
            });
        let default_available =
            self.miner_params.is_some() && (storage.is_some() || self.network.is_regtest());
        // Poll even under override load: due refreshes and error recovery must make progress.
        let elapsed = self.refresh.as_mut().poll(cx).is_ready();
        let prioritize_default = default_available && (elapsed || (needs_tip && !self.was_failing));

        // Bound override backlog independently of the transaction download queue. A cancelled
        // caller must not cause a new proof, but still let the next queued request make progress.
        let request = if retry_request.is_some() {
            retry_request
        } else if prioritize_default {
            None
        } else {
            match self.requests.poll_recv(cx) {
                Poll::Ready(Some(request)) => Some(request),
                Poll::Ready(None) | Poll::Pending => None,
            }
        };
        if request
            .as_ref()
            .is_some_and(|request| request.response.is_closed())
        {
            cx.waker().wake_by_ref();
            return Poll::Ready(Ok(()));
        }
        let is_override = request.is_some();
        // Ordinary same-tip changes cannot bypass or postpone the refresh/backstop deadline.
        if !is_override && !prioritize_default {
            return Poll::Ready(Ok(()));
        }

        let coinbase_only = !is_override && needs_tip;
        let fill_after_coinbase =
            coinbase_only && storage.is_some_and(|(storage, _)| !storage.transactions().is_empty());
        let snapshot = if coinbase_only {
            None
        } else {
            storage.map(|(storage, tip)| {
                (
                    tip,
                    storage.transactions().values().cloned().collect(),
                    storage.transaction_dependencies().clone(),
                )
            })
        };
        let miner_params = request
            .as_ref()
            .map(|request| request.miner_params.clone())
            .or_else(|| self.miner_params.clone())
            .expect("default builds require configured parameters; overrides supply their own");
        // Coinbase proofs are bound to payout, data, and memo. Overrides never share this cache
        // or consume the default miner's look-ahead proof.
        let coinbase_cache = if is_override {
            CoinbaseCache::default()
        } else {
            self.coinbase_cache.clone()
        };
        let next_coinbase = self.next_coinbase.take();
        self.build = Some(Build {
            future: build(
                self.network.clone(),
                miner_params,
                coinbase_cache,
                self.read_state.clone(),
                self.verifier.clone(),
                snapshot,
                next_coinbase,
                !is_override,
            )
            .boxed(),
            request,
            coinbase_only,
        });
        if !is_override {
            self.dirty = fill_after_coinbase;
        }
        // A newly stored future has not registered any wakeups yet.
        cx.waker().wake_by_ref();
        Poll::Ready(Ok(()))
    }
}

/// Build and validate a snapshot, returning retained look-ahead work even on failure.
#[allow(clippy::too_many_arguments)]
async fn build<ReadStateService, Verifier>(
    network: Network,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    read_state: ReadStateService,
    verifier: Verifier,
    snapshot: Option<Snapshot>,
    mut next_coinbase: Option<CoinbaseTask>,
    use_precomputed_coinbase: bool,
) -> (
    Result<Option<BlockTemplateResponse>, BoxError>,
    Option<CoinbaseTask>,
)
where
    ReadStateService: zebra_state::ReadState,
    Verifier: zebra_consensus::router::service_trait::BlockVerifierService,
{
    let result = async {
        let chain_info =
            timeout(BLOCK_VERIFY_TIMEOUT, fetch_chain_info(read_state.clone())).await??;
        let height = chain_info.tip_height.next()?;
        if NetworkUpgrade::current(&network, height) < NetworkUpgrade::Canopy
            || (chain_info.chain_history_root.is_none()
                && NetworkUpgrade::Heartwood.activation_height(&network) != Some(height))
        {
            return Err("block templates require post-Canopy chain information".into());
        }
        let (transactions, dependencies) = match snapshot {
            Some((tip, transactions, dependencies)) if tip == chain_info.tip_hash => {
                (transactions, dependencies)
            }
            Some(_) => return Ok(None),
            None => Default::default(),
        };
        if use_precomputed_coinbase {
            store_precomputed_coinbase(&mut next_coinbase, height, &coinbase_cache).await;
        }
        let long_poll_id = LongPollInput::new(
            chain_info.tip_height,
            chain_info.tip_hash,
            chain_info.max_time,
            transactions.iter().map(|tx| tx.transaction.id),
        )
        .generate_id();

        // Await the blocking task even across tip changes; dropping it cannot cancel proving.
        let (template, proposal) = tokio::task::spawn_blocking(move || {
            // Prepare coinbases fallibly before calling the synchronous selection/response
            // helpers, whose cached path assumes valid payout and monetary-range inputs.
            if coinbase_cache.get(height, Amount::zero()).is_none() {
                coinbase_cache.store(
                    height,
                    Amount::zero(),
                    TransactionTemplate::new_coinbase(
                        &network,
                        height,
                        &miner_params,
                        Amount::zero(),
                    )?,
                );
            }
            let selected = select_mempool_transactions(
                &network,
                height,
                &miner_params,
                transactions,
                dependencies,
                Some(&coinbase_cache),
            );
            let fees = selected
                .iter()
                .map(|tx| tx.miner_fee)
                .sum::<zebra_chain::amount::Result<Amount<NonNegative>>>()?;
            if coinbase_cache.get(height, fees).is_none() {
                coinbase_cache.store(
                    height,
                    fees,
                    TransactionTemplate::new_coinbase(&network, height, &miner_params, fees)?,
                );
            }
            let template = BlockTemplateResponse::from_transactions(
                &network,
                &coinbase_cache,
                &miner_params,
                &chain_info,
                long_poll_id,
                selected,
                None,
            );
            let proposal = proposal_block_from_template(&template, None, &network)?;
            if u64::try_from(proposal.zcash_serialized_size())? > MAX_BLOCK_BYTES {
                return Err::<_, BoxError>("block template exceeds the block size limit".into());
            }
            Ok((template, Arc::new(proposal)))
        })
        .await??;

        let check = verifier.oneshot(zebra_consensus::Request::CheckProposal(proposal));
        tokio::pin!(check);
        let validity = match timeout(BLOCK_VERIFY_TIMEOUT, &mut check).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "waiting for timed-out template verification to finish"
                );
                // A timeout cannot cancel cryptographic work already dispatched by consensus.
                // Keep the one build slot occupied until that same request drains.
                let _ = check.await;
                Err(error.into())
            }
        };
        let ReadResponse::Tip(tip) =
            timeout(BLOCK_VERIFY_TIMEOUT, read_state.oneshot(ReadRequest::Tip)).await??
        else {
            unreachable!("state service returned the wrong response to a Tip request");
        };
        if tip.map(|(_, hash)| hash) != Some(template.previous_block_hash()) {
            return Ok(None);
        }
        validity?;
        Ok(Some(template))
    }
    .await;
    (result, next_coinbase)
}

/// Retain an unfinished proof even when a reorg changes the desired height.
fn start_precomputing_coinbase(
    next_coinbase: &mut Option<CoinbaseTask>,
    network: &Network,
    miner_params: &MinerParams,
    height: Height,
) {
    if next_coinbase
        .as_ref()
        .is_some_and(|(precomputed_height, task)| {
            *precomputed_height == height || !task.is_finished()
        })
    {
        return;
    }
    let (network, miner_params) = (network.clone(), miner_params.clone());
    *next_coinbase = Some((
        height,
        tokio::task::spawn_blocking(move || {
            TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
                .expect("configured miner parameters and the next height produce a valid coinbase")
        }),
    ));
}

/// Only consume a look-ahead proof for its original height and miner parameters.
async fn store_precomputed_coinbase(
    next_coinbase: &mut Option<CoinbaseTask>,
    height: Height,
    coinbase_cache: &CoinbaseCache,
) {
    if next_coinbase
        .as_ref()
        .is_none_or(|(precomputed_height, _)| *precomputed_height != height)
    {
        return;
    }
    let (_, coinbase) = next_coinbase
        .take()
        .expect("the precomputed height was checked");
    match coinbase.await {
        Ok(coinbase) => coinbase_cache.store(height, Amount::zero(), coinbase),
        Err(error) => tracing::warn!(?error, "precomputed coinbase transaction task failed"),
    }
}
