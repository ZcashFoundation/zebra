//! Headers-first sync before a state snapshot bootstrap.
//!
//! A pruned node that bootstraps from a state snapshot starts like any other
//! node: it opens its state, starts the network, and syncs and verifies the
//! header chain from genesis. Once the header chain reaches the checkpoint
//! after the snapshot height `H`, it:
//! - checks the snapshot against those headers, instead of fetching headers
//!   for the bootstrap,
//! - downloads the snapshot from the version 2 peers that serve state
//!   snapshots, which it finds through peer discovery when none are
//!   configured, and
//! - replaces its header-only state with the snapshot, which keeps every
//!   header from genesis, but no body or block info below the snapshot window.
//!
//! Until then the node commits no block above genesis, so it downloads no
//! body it won't keep. If it can't bootstrap, it keeps the state it synced,
//! and syncs normally.

use std::{
    collections::HashSet,
    iter,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    sync::watch,
    time::{sleep, timeout, timeout_at, Instant},
};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, merkle, Block, Header, Height},
    chain_tip::ChainTip,
    parameters::Network,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
    transaction,
};
use zebra_network::{
    self as zn, sync_peer::ObjectChunk, types::PeerServices, InventoryResponse, SyncPeer, SyncPeers,
};
use zebra_state::{self as zs, LatestChainTip};

use crate::{
    components::state_snapshot::{
        bootstrap::{self, BootstrapError, StagedSnapshot},
        progress::Stage,
    },
    config::ZebradConfig,
    BoxError,
};

#[cfg(test)]
mod tests;

/// How long the header chain can go without growing before the node gives up
/// on the bootstrap and syncs normally.
pub const HEADER_STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the node waits, once its header chain reaches the snapshot, for a
/// connected version 2 peer that serves state snapshots.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the node logs its header sync progress while it waits.
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// How long the node waits between attempts to download the genesis block.
const GENESIS_RETRY_DELAY: Duration = Duration::from_secs(5);

/// How long the state can take to commit the genesis block.
const GENESIS_COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// The most discovered peers the bootstrap uses.
const MAX_DISCOVERED_PEERS: usize = 8;

/// Syncs headers to the checkpoint after the snapshot, finds peers that serve
/// it, and downloads, checks and stages the snapshot, for the caller to install
/// once it has closed `state`.
///
/// `configured_peers` are the bootstrap peers from the config; when there are
/// none, the bootstrap uses the connected version 2 peers in `sync_peers` that
/// advertise `NODE_SYNC_ARTIFACTS` and hold the snapshot's manifest.
/// `header_tip` is headers-first sync's tip.
#[allow(clippy::too_many_arguments)]
pub(super) async fn sync_headers_and_stage<ZN, ZS>(
    config: &ZebradConfig,
    commitment: StateSnapshotCommitment,
    configured_peers: Vec<String>,
    peer_set: ZN,
    state: ZS,
    read_state: zs::ReadStateService,
    latest_chain_tip: LatestChainTip,
    mut header_tip: watch::Receiver<Option<Height>>,
    sync_peers: SyncPeers,
) -> Result<StagedSnapshot, BootstrapError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send + 'static,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    let network = &config.network.network;
    let checkpoint = checkpoint_after(network, commitment.height)?;
    info!(
        snapshot_height = commitment.height.0,
        checkpoint = checkpoint.0 .0,
        "syncing and verifying headers from genesis before the state snapshot bootstrap: the \
         node downloads no block body until it has bootstrapped or given up"
    );

    // Headers-first sync starts from the genesis block, which the syncer would commit first.
    if latest_chain_tip.best_tip_height().is_none() {
        commit_genesis(peer_set, state, network).await?;
    }

    wait_for_header_tip(&mut header_tip, checkpoint.0).await?;

    // Discovered peers are already connected, and usually allow one connection per IP address,
    // so the bootstrap uses those connections. Configured peers are dialled.
    let discovered = if configured_peers.is_empty() {
        Some(discover_snapshot_peers(sync_peers, commitment.manifest).await?)
    } else {
        None
    };

    let synced_headers = synced_headers(read_state, commitment.height, checkpoint).await?;

    info!(
        height = commitment.height.0,
        hash = %commitment.block_hash,
        manifest = %commitment.manifest,
        peers = discovered.as_ref().map_or(configured_peers.len(), Vec::len),
        "{}",
        Stage::BootstrapStarted
    );
    match discovered {
        Some(peers) => {
            bootstrap::bootstrap_from_connected(config, commitment, peers, Some(synced_headers))
                .await
        }
        None => {
            bootstrap::bootstrap(config, commitment, configured_peers, Some(synced_headers)).await
        }
    }
}

/// The first checkpoint above the snapshot height `height`.
fn checkpoint_after(
    network: &Network,
    height: Height,
) -> Result<(Height, block::Hash), BootstrapError> {
    let checkpoints = network.checkpoint_list();
    let successor = (height + 1).ok_or_else(|| {
        BootstrapError::Aborted("the snapshot height has no successor".to_string())
    })?;
    let checkpoint = checkpoints
        .min_height_in_range(successor..)
        .ok_or_else(|| {
            BootstrapError::Aborted("no checkpoint above the snapshot height".to_string())
        })?;
    let hash = checkpoints
        .hash(checkpoint)
        .expect("a height from the checkpoint list has a hash");

    Ok((checkpoint, hash))
}

/// Downloads the genesis block from `peer_set` and commits it to `state`,
/// retrying until a peer serves it, for up to [`HEADER_STALL_TIMEOUT`]. On
/// Regtest, commits the Regtest genesis block, which Zebra has.
///
/// The genesis hash is the network's, so a block with that hash and a matching
/// Merkle root is the genesis block.
async fn commit_genesis<ZN, ZS>(
    peer_set: ZN,
    state: ZS,
    network: &Network,
) -> Result<(), BootstrapError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send + 'static,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    let genesis_hash = network.genesis_hash();
    let deadline = Instant::now() + HEADER_STALL_TIMEOUT;

    let mut known_genesis = network
        .is_regtest()
        .then(regtest_genesis_block)
        .filter(|block| is_genesis(block, genesis_hash));

    loop {
        let request = zn::Request::BlocksByHash(iter::once(genesis_hash).collect());
        let block = if let Some(block) = known_genesis.take() {
            Some(block)
        } else {
            match timeout(
                zn::constants::REQUEST_TIMEOUT,
                peer_set.clone().oneshot(request),
            )
            .await
            {
                Ok(Ok(zn::Response::Blocks(blocks))) => {
                    blocks.into_iter().find_map(|block| match block {
                        InventoryResponse::Available((block, _))
                            if is_genesis(&block, genesis_hash) =>
                        {
                            Some(block)
                        }
                        _ => None,
                    })
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => None,
            }
        };

        if let Some(block) = block {
            let request = zs::Request::CommitCheckpointVerifiedBlock(
                zs::CheckpointVerifiedBlock::from(block),
            );
            match timeout(GENESIS_COMMIT_TIMEOUT, state.clone().oneshot(request)).await {
                Ok(Ok(_)) => {
                    info!("committed the genesis block, so headers-first sync can start");
                    return Ok(());
                }
                Ok(Err(error)) => debug!(%error, "could not commit the genesis block"),
                Err(_) => debug!("committing the genesis block timed out"),
            }
        }

        if Instant::now() >= deadline {
            return Err(BootstrapError::Aborted(
                "no peer served the genesis block, so the node could not sync headers".to_string(),
            ));
        }
        sleep(GENESIS_RETRY_DELAY).await;
    }
}

/// Returns whether `block` is the genesis block with hash `genesis_hash`.
fn is_genesis(block: &Block, genesis_hash: block::Hash) -> bool {
    let root: merkle::Root = block.transactions.iter().collect();

    block.hash() == genesis_hash && root == block.header.merkle_root
}

/// Waits until the header chain tip reaches `target`, failing once it has not
/// moved for [`HEADER_STALL_TIMEOUT`].
pub(super) async fn wait_for_header_tip(
    header_tip: &mut watch::Receiver<Option<Height>>,
    target: Height,
) -> Result<(), BootstrapError> {
    let mut last_log = Instant::now();
    loop {
        let tip = *header_tip.borrow_and_update();
        if tip.is_some_and(|tip| tip >= target) {
            info!(
                header_tip = ?tip.map(|tip| tip.0),
                "the header chain reached the checkpoint after the state snapshot"
            );
            return Ok(());
        }
        if last_log.elapsed() >= PROGRESS_LOG_INTERVAL {
            last_log = Instant::now();
            info!(
                header_tip = ?tip.map(|tip| tip.0),
                target = target.0,
                "syncing headers before the state snapshot bootstrap"
            );
        }

        match timeout(HEADER_STALL_TIMEOUT, header_tip.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                return Err(BootstrapError::Aborted(
                    "headers-first sync stopped before it reached the snapshot".to_string(),
                ))
            }
            Err(_) => {
                return Err(BootstrapError::Aborted(format!(
                    "the header chain stayed at {:?} for {HEADER_STALL_TIMEOUT:?}, below the \
                     checkpoint at {} after the snapshot",
                    tip.map(|tip| tip.0),
                    target.0
                )))
            }
        }
    }
}

/// Returns the connected version 2 peers that hold the snapshot's `manifest`,
/// one per IP address, waiting up to [`DISCOVERY_TIMEOUT`] for the first.
///
/// Every version 2 node advertises `NODE_SYNC_ARTIFACTS`, including the many
/// that serve no state snapshot, so each connected peer that advertises it is
/// asked for the first byte of the manifest, once per connection. Peers that
/// connect later are asked as they connect, until one holds it.
pub(super) async fn discover_snapshot_peers(
    mut sync_peers: SyncPeers,
    manifest: ObjectHash,
) -> Result<Vec<SyncPeer>, BootstrapError> {
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    let mut asked = HashSet::new();

    loop {
        let candidates: Vec<SyncPeer> = sync_peers
            .borrow_and_update()
            .iter()
            .filter(|peer| {
                !peer.is_closed()
                    && peer.services().contains(PeerServices::NODE_SYNC_ARTIFACTS)
                    && !asked.contains(&peer.addr())
            })
            .cloned()
            .collect();
        asked.extend(candidates.iter().map(SyncPeer::addr));

        let holders = futures::future::join_all(
            candidates
                .into_iter()
                .map(|peer| async move { holds_object(&peer, manifest).await.then_some(peer) }),
        )
        .await;
        let mut seen = HashSet::new();
        let holders: Vec<SyncPeer> = holders
            .into_iter()
            .flatten()
            .filter(|peer| seen.insert(peer.addr().ip()))
            .take(MAX_DISCOVERED_PEERS)
            .collect();

        if !holders.is_empty() {
            info!(
                peers = ?holders.iter().map(SyncPeer::addr).collect::<Vec<_>>(),
                asked = asked.len(),
                "found connected version 2 peers that serve state snapshots"
            );
            return Ok(holders);
        }

        match timeout_at(deadline, sync_peers.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => {
                return Err(BootstrapError::Aborted(format!(
                    "none of the {} connected version 2 peers that advertised state snapshot \
                     service held the snapshot's manifest within {DISCOVERY_TIMEOUT:?}, and \
                     sync.state_snapshot_peers is empty",
                    asked.len()
                )))
            }
        }
    }
}

/// Returns whether `peer` serves the object `hash`, by asking it for the first byte.
async fn holds_object(peer: &SyncPeer, hash: ObjectHash) -> bool {
    match peer.get_object(hash, 0, 1).await {
        Ok(ObjectChunk::Found { .. }) => true,
        Ok(ObjectChunk::NotFound) => false,
        Err(error) => {
            debug!(peer = %peer.addr(), %error, "a version 2 peer didn't say if it serves an object");
            false
        }
    }
}

/// Reads the synced headers from the snapshot height `height` to `checkpoint`.
async fn synced_headers(
    read_state: zs::ReadStateService,
    height: Height,
    checkpoint: (Height, block::Hash),
) -> Result<Vec<Arc<Header>>, BootstrapError> {
    let count = u32::try_from(checkpoint.0 - height)
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| {
            BootstrapError::Aborted("the checkpoint is below the snapshot height".to_string())
        })?;
    let request = zs::ReadRequest::HeaderChainRange {
        start: height,
        count,
    };
    match read_state.oneshot(request).await {
        Ok(zs::ReadResponse::HeaderChainRange(headers)) => Ok(headers),
        Ok(response) => unreachable!("unexpected response to HeaderChainRange: {response:?}"),
        Err(error) => Err(BootstrapError::Aborted(format!(
            "could not read the synced headers: {error}"
        ))),
    }
}

/// A [`ChainTip`] that follows the latest chain tip of the state the node has
/// open, so the network keeps running when a state snapshot bootstrap replaces
/// the state.
#[derive(Clone, Debug)]
pub(super) struct ReopenableChainTip {
    /// The chain tip of each state the node opened, the latest last.
    tips: watch::Receiver<LatestChainTip>,

    /// The chain tip whose changes [`ChainTip::best_tip_changed`] waits for.
    current: LatestChainTip,

    /// Whether the node is syncing headers for a state snapshot bootstrap.
    bootstrapping: Arc<AtomicBool>,
}

/// Switches a [`ReopenableChainTip`] to a reopened state, and ends its bootstrap.
#[derive(Debug)]
pub(super) struct ChainTipSwitch {
    tips: watch::Sender<LatestChainTip>,
    bootstrapping: Arc<AtomicBool>,
}

impl ReopenableChainTip {
    /// Returns a chain tip that follows `tip`, and its switch. While `bootstrapping`,
    /// the tip counts as near the network tip.
    pub(super) fn new(tip: LatestChainTip, bootstrapping: bool) -> (ChainTipSwitch, Self) {
        let (sender, tips) = watch::channel(tip.clone());
        let bootstrapping = Arc::new(AtomicBool::new(bootstrapping));
        (
            ChainTipSwitch {
                tips: sender,
                bootstrapping: bootstrapping.clone(),
            },
            ReopenableChainTip {
                tips,
                current: tip,
                bootstrapping,
            },
        )
    }
}

impl ChainTipSwitch {
    /// Switches the chain tip to `tip`, the tip of a reopened state.
    pub(super) fn reopen(&self, tip: LatestChainTip) {
        self.tips.send_replace(tip);
    }

    /// Ends the bootstrap, so the chain tip counts as near the network tip only when it is.
    pub(super) fn end_bootstrap(&self) {
        self.bootstrapping.store(false, Ordering::Relaxed);
    }
}

impl ChainTip for ReopenableChainTip {
    fn best_tip_height(&self) -> Option<Height> {
        self.tips.borrow().best_tip_height()
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        self.tips.borrow().best_tip_hash()
    }

    fn best_tip_height_and_hash(&self) -> Option<(Height, block::Hash)> {
        self.tips.borrow().best_tip_height_and_hash()
    }

    fn best_tip_block_time(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.tips.borrow().best_tip_block_time()
    }

    fn best_tip_changed_at(&self, expected_hash: block::Hash) -> Option<std::time::Instant> {
        self.tips.borrow().best_tip_changed_at(expected_hash)
    }

    fn best_tip_height_and_block_time(&self) -> Option<(Height, chrono::DateTime<chrono::Utc>)> {
        self.tips.borrow().best_tip_height_and_block_time()
    }

    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        self.tips.borrow().best_tip_mined_transaction_ids()
    }

    async fn best_tip_changed(&mut self) -> Result<(), zebra_chain::BoxError> {
        /// What the tip did.
        enum Next {
            Changed,
            Closed(zebra_chain::BoxError),
            Reopened,
        }

        // A reopened state's tip counts as a change.
        let next = tokio::select! {
            changed = self.current.best_tip_changed() => match changed {
                Ok(()) => Next::Changed,
                Err(error) => Next::Closed(error),
            },
            reopened = self.tips.changed() => match reopened {
                Ok(()) => Next::Reopened,
                Err(error) => Next::Closed(error.into()),
            },
        };

        match next {
            Next::Changed => {}
            // The state closes before a bootstrap replaces it, so wait for the new one.
            Next::Closed(error) => {
                self.tips.changed().await.map_err(|_| error)?;
                self.current = self.tips.borrow_and_update().clone();
            }
            Next::Reopened => self.current = self.tips.borrow_and_update().clone(),
        }

        Ok(())
    }

    /// A node that syncs headers for a state snapshot bootstrap downloads no old block, so the
    /// network doesn't require its outbound peers to serve old blocks: every peer serves
    /// headers, and the snapshot node it becomes syncs only recent blocks.
    fn is_at_or_near_network_tip(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.bootstrapping.load(Ordering::Relaxed)
            || self.tips.borrow().is_at_or_near_network_tip(now)
    }

    fn mark_best_tip_seen(&mut self) {
        if self.tips.has_changed().unwrap_or(false) {
            self.current = self.tips.borrow_and_update().clone();
        }
        self.current.mark_best_tip_seen();
    }
}
