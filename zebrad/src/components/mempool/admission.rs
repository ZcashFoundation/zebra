//! A single retained, non-publishing proposal check before verified-set admission.

use std::{
    collections::HashSet,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use tokio::sync::oneshot;
use tower::ServiceExt;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, MAX_BLOCK_BYTES},
    parameters::Network,
    serialization::ZcashSerialize,
    transaction::{Hash, UnminedTxId, VerifiedUnminedTx},
    transparent::OutPoint,
};
use zebra_rpc::{
    config::mining::{self, default_miner_address, MinerAddressType},
    proposal_block_from_template, BlockTemplateResponse, CoinbaseCache, LongPollInput, MinerParams,
    TransactionTemplate,
};
use zebra_state::{ReadRequest, ReadResponse};

use super::{
    block_template::{BlockVerifier, ReadState},
    downloads::TRANSACTION_VERIFY_TIMEOUT,
    BoxError, Storage,
};

/// Proposal validation owns its transaction and response until the mempool consumes the result.
pub(super) struct Pending {
    /// The individually verified transaction, still invisible to mempool consumers.
    pub tx: VerifiedUnminedTx,
    /// Its direct mempool spends, rechecked by final storage insertion.
    pub spent: Vec<OutPoint>,
    /// The committed parent required by this proposal.
    pub parent: block::Hash,
    /// The original queue caller, held through stale retries.
    pub response: Option<oneshot::Sender<Result<(), BoxError>>>,
    ancestors: Vec<UnminedTxId>,
    stale: bool,
    work: BoxFuture<'static, Result<bool, BoxError>>,
}

/// One validation context, independent of the configured mining payout and template cache.
pub(super) struct Admission {
    network: Network,
    read_state: ReadState,
    verifier: BlockVerifier,
    miner_params: MinerParams,
    coinbase_cache: CoinbaseCache,
    /// At most one proposal, retained until its blocking verification work has finished.
    pub pending: Option<Pending>,
}

impl Admission {
    /// Create the private transparent validation context, even on non-mining nodes.
    pub fn new(network: Network, read_state: ReadState, verifier: BlockVerifier) -> Self {
        let miner_params = MinerParams::new(
            &network,
            mining::Config {
                miner_address: Some(
                    default_miner_address(network.kind(), &MinerAddressType::Transparent)
                        .parse()
                        .expect("the built-in validation address is valid"),
                ),
                ..Default::default()
            },
        )
        .expect("the transparent validation payout matches the network");
        Self {
            network,
            read_state,
            verifier,
            miner_params,
            coinbase_cache: CoinbaseCache::default(),
            pending: None,
        }
    }

    /// Mark work stale, but retain it until all proposal/proof work actually finishes.
    pub fn reset(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.stale = true;
        }
    }

    /// Snapshot a complete bounded package without inserting or publishing the candidate.
    pub fn start(
        &mut self,
        storage: &Storage,
        parent: block::Hash,
        tx: VerifiedUnminedTx,
        spent: Vec<OutPoint>,
        response: Option<oneshot::Sender<Result<(), BoxError>>>,
    ) {
        assert!(
            self.pending.is_none(),
            "only one proposal admission can run"
        );
        let stale = !required_outputs_available(storage, &spent);
        let package = required_package(storage, &tx, &spent);
        let ancestors = package
            .as_ref()
            .map(|txs| {
                txs.iter()
                    .take(txs.len() - 1)
                    .map(|tx| tx.transaction.id)
                    .collect()
            })
            .unwrap_or_default();
        let network = self.network.clone();
        let read_state = self.read_state.clone();
        let verifier = self.verifier.clone();
        let miner_params = self.miner_params.clone();
        let cache = self.coinbase_cache.clone();
        let work = verify_package(
            network,
            read_state,
            verifier,
            miner_params,
            cache,
            package,
            parent,
        )
        .boxed();
        self.pending = Some(Pending {
            tx,
            spent,
            parent,
            response,
            ancestors,
            stale,
            work,
        });
    }

    /// A ready result is still checked against the reconciled storage before final insertion.
    pub fn poll(
        &mut self,
        cx: &mut Context<'_>,
        storage: &Storage,
        parent: block::Hash,
    ) -> Poll<(Pending, Result<bool, BoxError>)> {
        let Some(pending) = &mut self.pending else {
            return Poll::Pending;
        };
        let Poll::Ready(mut result) = pending.work.as_mut().poll(cx) else {
            return Poll::Pending;
        };
        if pending.stale
            || pending.parent != parent
            || pending.ancestors.iter().any(|id| {
                storage
                    .transactions()
                    .get(&id.mined_id())
                    .map(|tx| tx.transaction.id)
                    != Some(*id)
            })
        {
            result = Ok(false);
        }
        Poll::Ready((
            self.pending
                .take()
                .expect("the completed admission is present"),
            result,
        ))
    }
}

/// Return the complete mandatory ancestor closure followed by the candidate, never a selection.
pub(super) fn required_package(
    storage: &Storage,
    candidate: &VerifiedUnminedTx,
    spent: &[OutPoint],
) -> Result<Vec<VerifiedUnminedTx>, BoxError> {
    if !required_outputs_available(storage, spent) {
        return Err("a required mempool output is no longer available".into());
    }
    let mut bytes = candidate.transaction.size;
    let mut sigops = candidate.block_sigop_count();
    check_limits(bytes, sigops)?;
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut stack: Vec<(Hash, bool)> = spent
        .iter()
        .map(|outpoint| (outpoint.hash, false))
        .collect();
    let mut package = Vec::new();
    while let Some((hash, expanded)) = stack.pop() {
        if expanded {
            visiting.remove(&hash);
            visited.insert(hash);
            package.push(storage.transactions()[&hash].clone());
            continue;
        }
        if visited.contains(&hash) {
            continue;
        }
        if hash == candidate.transaction.id.mined_id() || !visiting.insert(hash) {
            return Err("cyclic transaction dependencies cannot form a block".into());
        }
        let tx = storage
            .transactions()
            .get(&hash)
            .ok_or("a required ancestor is no longer available")?;
        bytes = bytes
            .checked_add(tx.transaction.size)
            .ok_or("package byte count overflow")?;
        sigops = sigops
            .checked_add(tx.block_sigop_count())
            .ok_or("package sigop count overflow")?;
        check_limits(bytes, sigops)?;
        stack.push((hash, true));
        if let Some(dependencies) = storage.transaction_dependencies().dependencies().get(&hash) {
            stack.extend(dependencies.iter().map(|hash| (*hash, false)));
        }
    }
    package.push(candidate.clone());
    Ok(package)
}

fn required_outputs_available(storage: &Storage, spent: &[OutPoint]) -> bool {
    spent.iter().all(|outpoint| {
        storage
            .transactions()
            .get(&outpoint.hash)
            .is_some_and(|tx| {
                usize::try_from(outpoint.index)
                    .ok()
                    .is_some_and(|index| tx.transaction.transaction.outputs().get(index).is_some())
            })
    })
}

fn check_limits(bytes: usize, sigops: u32) -> Result<(), BoxError> {
    if u64::try_from(bytes)? > MAX_BLOCK_BYTES || sigops > zebra_consensus::MAX_BLOCK_SIGOPS {
        return Err("required transaction package exceeds block byte or sigop limits".into());
    }
    Ok(())
}

/// Check a package against one committed parent, without publishing or committing it.
async fn verify_package<ReadStateService, Verifier>(
    network: Network,
    read_state: ReadStateService,
    verifier: Verifier,
    miner_params: MinerParams,
    cache: CoinbaseCache,
    package: Result<Vec<VerifiedUnminedTx>, BoxError>,
    parent: block::Hash,
) -> Result<bool, BoxError>
where
    ReadStateService: zebra_state::ReadState,
    Verifier: zebra_consensus::router::service_trait::BlockVerifierService,
{
    let package = package?;
    let chain_info = match tokio::time::timeout(
        TRANSACTION_VERIFY_TIMEOUT,
        read_state.clone().oneshot(ReadRequest::ChainInfo),
    )
    .await??
    {
        ReadResponse::ChainInfo(info) => info,
        _ => unreachable!("ChainInfo requests return ChainInfo responses"),
    };
    if chain_info.tip_hash != parent {
        return Ok(false);
    }
    let proposal = tokio::task::spawn_blocking(move || -> Result<_, BoxError> {
        let height = chain_info.tip_height.next()?;
        if zebra_chain::parameters::NetworkUpgrade::current(&network, height)
            < zebra_chain::parameters::NetworkUpgrade::Canopy
            || (chain_info.chain_history_root.is_none()
                && zebra_chain::parameters::NetworkUpgrade::Heartwood.activation_height(&network)
                    != Some(height))
        {
            return Err("proposal admission requires post-Canopy chain information".into());
        }
        let fees = package
            .iter()
            .map(|tx| tx.miner_fee)
            .sum::<zebra_chain::amount::Result<Amount<NonNegative>>>()?;
        // Use the fallible constructor before the infallible template helper, including
        // checked fee/subsidy arithmetic. This cache never contains a mining payout.
        if cache.get(height, fees).is_none() {
            cache.store(
                height,
                fees,
                TransactionTemplate::new_coinbase(&network, height, &miner_params, fees)?,
            );
        }
        let id = LongPollInput::new(
            chain_info.tip_height,
            parent,
            chain_info.max_time,
            package.iter().map(|tx| tx.transaction.id),
        )
        .generate_id();
        let template = BlockTemplateResponse::from_transactions(
            &network,
            &cache,
            &miner_params,
            &chain_info,
            id,
            package,
            None,
        );
        let proposal = proposal_block_from_template(&template, None, &network)?;
        if u64::try_from(proposal.zcash_serialized_size()).expect("block sizes fit u64")
            > MAX_BLOCK_BYTES
        {
            return Err("required transaction package exceeds the block byte limit".into());
        }
        Ok(Arc::new(proposal))
    })
    .await??;

    // ponytail: recheck one block-bounded package; reuse proofs only through a future
    // consensus API bound to the exact witnessed transactions and committed parent.
    let check = verifier.oneshot(zebra_consensus::Request::CheckProposal(proposal));
    tokio::pin!(check);
    let result = match tokio::time::timeout(TRANSACTION_VERIFY_TIMEOUT, &mut check).await {
        Ok(result) => result.map(|_| ()),
        Err(error) => {
            // Dropping a verifier future does not stop its blocking cryptographic work.
            // Drain the same request before freeing the single admission slot.
            let _ = check.await;
            Err(error.into())
        }
    };
    let committed_parent = match tokio::time::timeout(
        TRANSACTION_VERIFY_TIMEOUT,
        read_state.oneshot(ReadRequest::Tip),
    )
    .await??
    {
        ReadResponse::Tip(tip) => tip.map(|(_, hash)| hash),
        _ => unreachable!("Tip requests return Tip responses"),
    };
    if committed_parent != Some(parent) {
        return Ok(false);
    }
    result?;
    Ok(true)
}
