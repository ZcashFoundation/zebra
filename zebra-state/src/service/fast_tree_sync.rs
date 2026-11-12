//! Fast checkpoint sync with peer-supplied note commitment tree roots.
//!
//! Updating the Sapling, Orchard, and Ironwood note commitment trees is most of the work of
//! committing checkpoint-verified blocks with many shielded outputs. Up to the height of the
//! trees embedded in Zebra, the block writer can instead take each block's tree roots from peers,
//! verify them against the chain's header commitments, and leave the trees as they are. At that
//! height, it takes the embedded trees, verified against that block's roots, and updates the trees
//! as usual from there.
//!
//! Before Heartwood, each block header commits to the Sapling root after its own block. From
//! Heartwood, each header commits to the chain history tree before its block, which commits to
//! the roots after each earlier block, so the roots after a block are verified by the next block.
//! Pools that have not activated have empty trees.
//!
//! The database has no note commitment trees for the heights committed this way, only their
//! roots and tree sizes. It gets the completed subtrees from the embedded trees.

use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::SocketAddr,
    ops::RangeInclusive,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use zebra_chain::{
    block::{self, Height},
    history_tree::{HistoryTree, HistoryTreeError},
    orchard,
    parallel::tree::{NoteCommitmentTrees, TreeRoots, TreeRootsProof},
    parameters::{Network, NetworkUpgrade},
    sapling,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
};

use crate::{
    request::FinalizableBlock,
    service::{
        check,
        finalized_state::{FastSyncTreeRoots, FromDisk, IntoDisk, ZebraDb},
    },
    CheckpointVerifiedBlock, ValidateContextError,
};

#[cfg(test)]
mod tests;

/// How fast checkpoint sync committed a block's note commitment trees.
#[derive(Clone, Debug)]
pub(crate) enum FastTreeSync {
    /// The trees are unchanged; the database stores the block's verified roots and the tree sizes
    /// after it instead.
    Frozen(FastSyncTreeRoots),
    /// The trees are the embedded trees, and the database also stores the subtrees they
    /// completed.
    Embedded(Arc<EmbeddedTrees>),
}

/// How many heights ahead of the block writer fast sync keeps roots for.
pub const TREE_ROOTS_WINDOW: u32 = 20_000;

/// The embedded Mainnet note commitment trees and subtrees.
const MAINNET_TREES: &[u8] = include_bytes!("fast_tree_sync/mainnet-trees.bin");

/// A bounded batch of unverified roots and the peer that supplied them.
pub(crate) type TreeRootsMessage = (Height, Vec<TreeRootsProof>, SocketAddr);

/// The next range of roots wanted by the writer.
pub(crate) type WantedTreeRoots = Option<RangeInclusive<Height>>;

/// Unverified roots owned exclusively by the block writer.
#[derive(Debug)]
pub(crate) struct TreeRootsCache {
    roots: BTreeMap<Height, (TreeRootsProof, SocketAddr)>,
    next_height: Height,
    first_missing: Height,
    trees: Option<Arc<EmbeddedTrees>>,
    first_height: Height,
    supplied: mpsc::Receiver<TreeRootsMessage>,
    sender: mpsc::SyncSender<TreeRootsMessage>,
    wanted_sender: tokio::sync::watch::Sender<WantedTreeRoots>,
    rejection_sender: mpsc::SyncSender<SocketAddr>,
    rejected_sources: Option<mpsc::Receiver<SocketAddr>>,
    rejected: Option<SocketAddr>,
}

impl TreeRootsCache {
    /// Creates the writer-owned cache and its bounded input channel.
    pub(crate) fn new(trees: Option<Arc<EmbeddedTrees>>, next_height: Height) -> Self {
        let (sender, supplied) = mpsc::sync_channel(1);
        let (wanted_sender, _) = tokio::sync::watch::channel(None);
        let (rejection_sender, rejected_sources) = mpsc::sync_channel(
            usize::try_from(TREE_ROOTS_WINDOW + 1).expect("the roots window fits in usize"),
        );
        let mut cache = Self {
            roots: BTreeMap::new(),
            next_height,
            first_missing: next_height,
            trees: trees.filter(|trees| trees.height >= next_height),
            first_height: Height(0),
            supplied,
            sender,
            wanted_sender,
            rejected: None,
            rejection_sender,
            rejected_sources: Some(rejected_sources),
        };
        cache.publish();
        cache
    }

    /// Returns the state service's input and progress channels.
    pub(crate) fn channels(
        &mut self,
    ) -> (
        mpsc::SyncSender<TreeRootsMessage>,
        tokio::sync::watch::Receiver<WantedTreeRoots>,
        mpsc::Receiver<SocketAddr>,
    ) {
        (
            self.sender.clone(),
            self.wanted_sender.subscribe(),
            self.rejected_sources
                .take()
                .expect("the state takes rejection reports once"),
        )
    }

    fn embedded_trees(&self) -> Option<Arc<EmbeddedTrees>> {
        self.trees.clone()
    }

    fn receive(&mut self, (start_height, roots, source): TreeRootsMessage) {
        let Some(window) = self.window() else { return };
        if self.rejected == Some(source) {
            return;
        }
        for (height, roots) in (start_height.0..).map(Height).zip(roots) {
            if height > *window.end() {
                break;
            }
            if window.contains(&height) {
                self.roots.entry(height).or_insert((roots, source));
            }
        }
        self.publish();
    }

    #[cfg(test)]
    fn supply(&mut self, start_height: Height, roots: Vec<TreeRootsProof>) {
        self.receive((start_height, roots, "127.0.0.1:8233".parse().unwrap()));
    }

    fn wanted(&self) -> Option<RangeInclusive<Height>> {
        let window = self.window()?;
        let first = (window.start().0.max(self.first_missing.0)..=window.end().0)
            .map(Height)
            .find(|height| !self.roots.contains_key(height))?;
        let last =
            self.roots
                .range(first..=*window.end())
                .next()
                .map_or(*window.end(), |(height, _)| {
                    height
                        .previous()
                        .expect("heights above a missing height are positive")
                });
        Some(first..=last)
    }

    fn publish(&mut self) {
        let wanted = self.wanted();
        self.first_missing = wanted
            .as_ref()
            .map(|range| *range.start())
            .or_else(|| *self.window()?.end() + 1)
            .unwrap_or(self.next_height);
        self.wanted_sender.send_replace(wanted);
    }

    fn set_next_height(&mut self, next_height: Height) {
        self.next_height = next_height;
        self.roots = self.roots.split_off(&next_height);
        if self
            .trees
            .as_ref()
            .is_some_and(|trees| trees.height < next_height)
        {
            self.trees = None;
            self.roots.clear();
        }
        self.publish();
    }

    fn wait_for_roots(&mut self, height: Height, timeout: Duration) -> Option<TreeRootsProof> {
        let deadline = Instant::now() + timeout;
        while self.roots(height).is_none() {
            if Instant::now() >= deadline {
                return None;
            }
            let message = self
                .supplied
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok()?;
            self.receive(message);
        }
        self.roots(height)
    }

    fn roots(&mut self, height: Height) -> Option<TreeRootsProof> {
        if let Ok(message) = self.supplied.try_recv() {
            self.receive(message);
        }
        self.roots.get(&height).map(|(roots, _)| *roots)
    }

    /// Rejects only unverified roots from the authenticated bad-root source.
    fn reject(&mut self, height: Height) {
        let source = self.roots[&height].1;
        self.roots.retain(|_, (_, peer)| *peer != source);
        self.rejected = Some(source);
        self.first_missing = self.next_height;
        // Publish replacement demand before the fetcher observes the rejected source.
        self.publish();
        // The sole fetcher reads one report before every batch it supplies. Outstanding reports
        // are bounded by the cached roots plus the single pending batch.
        if let Err(error) = self.rejection_sender.try_send(source) {
            assert!(
                matches!(error, mpsc::TrySendError::Disconnected(_)),
                "the roots window bounds outstanding rejection reports"
            );
        }
    }

    /// Ignores roots below the first height whose trees can be skipped.
    pub(crate) fn with_first_height(mut self, first_height: Height) -> Self {
        self.first_height = first_height;
        self.publish();
        self
    }

    fn window(&self) -> Option<RangeInclusive<Height>> {
        let end = self.trees.as_ref()?.height;
        let start = self.next_height.max(self.first_height);
        let end = (start + i64::from(TREE_ROOTS_WINDOW - 1))
            .map_or(end, |window_end| window_end.min(end));
        (start <= end).then_some(start..=end)
    }
}

/// Returns the trees fast sync ends with for a state with `db`, or `None` if it can't fast sync.
///
/// A state that stopped in the middle of fast sync must finish it. Otherwise, fast sync can only
/// start if `enabled`, and only once, so the fast-synced heights stay one range.
pub(crate) fn embedded_trees_for_state(
    trees: Option<Arc<EmbeddedTrees>>,
    db: &ZebraDb,
    enabled: bool,
    max_checkpoint_height: Height,
) -> Option<Arc<EmbeddedTrees>> {
    // The block after the embedded trees verifies their block's roots, so it must also be
    // checkpoint-verified.
    let trees = trees.filter(|trees| trees.height < max_checkpoint_height)?;

    match db.fast_sync_tree_heights() {
        Some(heights) if Some(*heights.end()) == db.finalized_tip_height() => Some(trees),
        Some(_) => None,
        None => enabled.then_some(trees),
    }
}

/// Bounds a frozen writer's wait, including after restarting with fast sync disabled.
#[cfg(not(test))]
const ROOTS_STALL_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(test)]
const ROOTS_STALL_TIMEOUT: Duration = Duration::from_secs(2);
const ROOTS_WAIT_LOG_INTERVAL: Duration = Duration::from_secs(1);

/// How the block writer commits a checkpoint-verified block.
pub(crate) enum CheckpointCommit {
    /// Commit the block with its verified tree roots, without updating its trees.
    Fast {
        /// The block, as a [`FinalizableBlock::FastCheckpoint`].
        block: Box<FinalizableBlock>,
        /// The tree sizes after the block if the trees stay frozen, or `None` if fast sync ends
        /// with the block.
        frozen_sizes: Option<[u64; 3]>,
        /// A malformed successor body, rejected separately from this authenticated block.
        successor_error: Option<ValidateContextError>,
    },
    /// Commit the block with tree updates.
    Normal(CheckpointVerifiedBlock),
    /// Frozen trees cannot advance because roots have been unavailable for the stall timeout.
    Stalled(Height),
    /// The state is shutting down, so the block won't be committed.
    Shutdown,
}

/// The block writer's fast checkpoint sync state.
pub(crate) struct FastTreeSyncWriter {
    /// The peer-supplied roots.
    cache: TreeRootsCache,
    /// The network.
    network: Network,
    /// The tree sizes after the last committed block, if the writer committed it without updating
    /// its trees.
    ///
    /// Until fast sync ends, the trees the writer has are older than the committed blocks, so it
    /// must not update trees for blocks.
    frozen_sizes: Option<[u64; 3]>,
}

impl FastTreeSyncWriter {
    /// Returns the fast sync state of a writer that commits to `db`.
    ///
    /// # Panics
    ///
    /// If `db` stopped in the middle of fast sync, and this Zebra version can't finish it.
    pub(crate) fn new(db: &ZebraDb, cache: TreeRootsCache, network: Network) -> Self {
        let frozen_sizes = db
            .fast_sync_tree_heights()
            .filter(|heights| Some(*heights.end()) == db.finalized_tip_height())
            .map(|heights| {
                let tip = db
                    .fast_sync_tree_roots(*heights.end())
                    .expect("fast-synced heights have stored roots");
                [tip.sapling_size, tip.orchard_size, tip.ironwood_size]
            });

        assert!(
            frozen_sizes.is_none() || cache.embedded_trees().is_some(),
            "the state stopped in the middle of fast checkpoint sync, and can't finish it: enable \
             `consensus.checkpoint_sync`, use a Zebra version with embedded trees above the state \
             tip, or delete the state cache directory and sync again",
        );

        Self {
            cache,
            network,
            frozen_sizes,
        }
    }

    /// Returns `true` if the writer should hold `block` until the next block arrives, which
    /// verifies the block's tree roots.
    pub(crate) fn defers(&mut self, block: &CheckpointVerifiedBlock) -> bool {
        let Some(trees) = self.cache.embedded_trees() else {
            return false;
        };
        let is_sapling_active = NetworkUpgrade::Sapling
            .activation_height(&self.network)
            .is_some_and(|activation| block.height >= activation);

        is_sapling_active
            && block.height <= trees.height
            && (self.frozen_sizes.is_some() || self.cache.roots(block.height).is_some())
    }

    /// Returns how to commit `block`, verifying its supplied tree roots against `next`, the block
    /// after it.
    ///
    /// `history_tree` is the chain history tree before `block`, and `trees` are the note
    /// commitment trees the writer has. Frozen trees wait boundedly for replacement roots.
    pub(crate) fn checkpoint_commit(
        &mut self,
        block: CheckpointVerifiedBlock,
        next: &CheckpointVerifiedBlock,
        history_tree: &Arc<HistoryTree>,
        trees: &NoteCommitmentTrees,
        is_shutting_down: impl Fn() -> bool,
    ) -> CheckpointCommit {
        let height = block.height;
        let deadline = Instant::now() + ROOTS_STALL_TIMEOUT;

        loop {
            if is_shutting_down() {
                return CheckpointCommit::Shutdown;
            }
            let proof = match self.cache.roots(height) {
                Some(proof) => proof,
                None if self.frozen_sizes.is_none() => return CheckpointCommit::Normal(block),
                None if Instant::now() >= deadline => return CheckpointCommit::Stalled(height),
                None => match self.cache.wait_for_roots(
                    height,
                    ROOTS_WAIT_LOG_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                ) {
                    Some(roots) => roots,
                    None => continue,
                },
            };

            let history_tree =
                match verify_tree_roots(&self.network, &block, &proof, history_tree, next) {
                    Ok(history_tree) => history_tree,
                    Err(error) => {
                        tracing::warn!(?height, %error, "rejected peer-supplied tree roots");
                        self.cache.reject(height);

                        // This source supplied the entire commitment preimage. An unrelated
                        // successor body's authorizing data cannot invalidate its proof.
                        continue;
                    }
                };
            let roots = proof.roots;
            let successor_error = check::block_commitment_is_valid_for_chain_history(
                next.block.clone(),
                &self.network,
                &history_tree,
                next.precomputed_auth_data_root(),
            )
            .err();

            let [sapling_size, orchard_size, ironwood_size] = self.sizes_after(&block, trees);
            let embedded_trees = self
                .cache
                .embedded_trees()
                .filter(|embedded_trees| embedded_trees.height == height);
            if let Some(embedded_trees) = &embedded_trees {
                embedded_trees
                    .check(&roots, [sapling_size, orchard_size, ironwood_size])
                    .expect("the embedded trees match the verified roots and tree sizes");
            }

            return CheckpointCommit::Fast {
                successor_error,
                frozen_sizes: embedded_trees.is_none().then_some([
                    sapling_size,
                    orchard_size,
                    ironwood_size,
                ]),
                block: Box::new(FinalizableBlock::FastCheckpoint {
                    checkpoint_verified: block,
                    tree_roots: FastSyncTreeRoots {
                        roots,
                        sapling_size,
                        orchard_size,
                        ironwood_size,
                    },
                    history_tree,
                    embedded_trees,
                }),
            };
        }
    }

    /// Returns `true` if the writer's trees are older than the committed blocks.
    pub(crate) fn is_frozen(&self) -> bool {
        self.frozen_sizes.is_some()
    }

    /// Records that the writer committed the block at `height`, and the tree sizes after it if
    /// the trees stay frozen.
    pub(crate) fn committed(&mut self, height: Height, frozen_sizes: Option<[u64; 3]>) {
        match (self.frozen_sizes.is_some(), frozen_sizes.is_some()) {
            (false, true) => tracing::info!(
                ?height,
                "fast checkpoint sync: committing blocks with verified peer-supplied note \
                 commitment tree roots"
            ),
            (true, false) => tracing::info!(
                ?height,
                "fast checkpoint sync: switched to the embedded note commitment trees"
            ),
            _ => {}
        }

        self.frozen_sizes = frozen_sizes;
        self.cache
            .set_next_height((height + 1).expect("committed heights are below the maximum"));
    }

    /// Returns the tree sizes after `block`.
    fn sizes_after(
        &self,
        block: &CheckpointVerifiedBlock,
        trees: &NoteCommitmentTrees,
    ) -> [u64; 3] {
        let [sapling, orchard, ironwood] = self.frozen_sizes.unwrap_or([
            trees.sapling.count(),
            trees.orchard.count(),
            trees.ironwood.count(),
        ]);
        let added = |count: usize| u64::try_from(count).expect("commitment counts fit in u64");

        [
            sapling + added(block.block.sapling_note_commitments().count()),
            orchard + added(block.block.orchard_note_commitments().count()),
            ironwood + added(block.block.ironwood_note_commitments().count()),
        ]
    }
}

/// An error that rejects peer-supplied note commitment tree roots.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TreeRootsError {
    /// A pool had a root other than the empty root before it activated.
    #[error("the {pool} root after block {height:?} is not empty before the pool activates")]
    InactivePool {
        /// The pool.
        pool: &'static str,
        /// The block height.
        height: Height,
    },

    /// The Sapling root differs from the one in the block's header.
    #[error("the Sapling root after block {height:?} differs from its header")]
    SaplingRoot {
        /// The block height.
        height: Height,
    },

    /// The next block's header commits to a different chain history.
    #[error("the block after {height:?} commits to different roots: {source}")]
    Commitment {
        /// The block height.
        height: Height,
        /// The commitment error.
        source: ValidateContextError,
    },

    /// A ZIP-244 successor header requires an independently supplied authorizing-data root.
    #[error("the proof after block {height:?} omits the successor authorizing-data root")]
    MissingAuthDataRoot {
        /// The height whose proof is incomplete.
        height: Height,
    },

    /// The embedded trees have different roots or sizes than the chain.
    #[error("the embedded note commitment trees don't match the chain at {height:?}")]
    EmbeddedTrees {
        /// The block height.
        height: Height,
    },
}

/// Verifies the supplier's complete `proof` against checkpoint-authenticated headers and returns
/// the chain history tree after `block`, without trusting `next`'s authorizing data.
///
/// `history_tree` is the history before `block`; `next` is its checkpoint-authenticated successor.
pub(crate) fn verify_tree_roots(
    network: &Network,
    block: &CheckpointVerifiedBlock,
    proof: &TreeRootsProof,
    history_tree: &Arc<HistoryTree>,
    next: &CheckpointVerifiedBlock,
) -> Result<Arc<HistoryTree>, TreeRootsError> {
    let height = block.height;
    let roots = &proof.roots;
    let empty = TreeRoots::empty();
    let is_active = |upgrade: NetworkUpgrade| {
        upgrade
            .activation_height(network)
            .is_some_and(|activation| height >= activation)
    };

    for (pool, upgrade, is_empty) in [
        (
            "Sapling",
            NetworkUpgrade::Sapling,
            roots.sapling == empty.sapling,
        ),
        (
            "Orchard",
            NetworkUpgrade::Nu5,
            roots.orchard == empty.orchard,
        ),
        (
            "Ironwood",
            NetworkUpgrade::Nu6_3,
            roots.ironwood == empty.ironwood,
        ),
    ] {
        if !is_active(upgrade) && !is_empty {
            return Err(TreeRootsError::InactivePool { pool, height });
        }
    }

    let commitment_error =
        |source: ValidateContextError| TreeRootsError::Commitment { height, source };

    if let block::Commitment::FinalSaplingRoot(header_root) = block
        .block
        .commitment(network)
        .map_err(|error| commitment_error(error.into()))?
    {
        if header_root != roots.sapling {
            return Err(TreeRootsError::SaplingRoot { height });
        }
    }

    let mut history_tree = history_tree.clone();
    Arc::make_mut(&mut history_tree)
        .push(
            network,
            block.block.clone(),
            roots.as_block_commitment_tree_roots(),
        )
        .map_err(|error: HistoryTreeError| commitment_error(Arc::new(error).into()))?;

    if let block::Commitment::ChainHistoryBlockTxAuthCommitment(actual) = next
        .block
        .commitment(network)
        .map_err(|error| commitment_error(error.into()))?
    {
        let auth_root = proof
            .successor_auth_data_root
            .ok_or(TreeRootsError::MissingAuthDataRoot { height })?;
        let history_root = history_tree.hash().unwrap_or_else(|| {
            assert_eq!(
                NetworkUpgrade::Heartwood.activation_height(network),
                Some(next.height),
                "only Heartwood activation has no preceding history root",
            );
            block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()
        });
        let expected = block::ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            &history_root,
            &auth_root,
        );
        if actual != expected {
            return Err(commitment_error(
                block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
                    actual: actual.into(),
                    expected: expected.into(),
                }
                .into(),
            ));
        }
    } else {
        check::block_commitment_is_valid_for_chain_history(
            next.block.clone(),
            network,
            &history_tree,
            None,
        )
        .map_err(commitment_error)?;
    }

    Ok(history_tree)
}

/// The Sapling, Orchard, and Ironwood note commitment trees after a checkpointed block, and the
/// subtrees they completed, which fast sync ends with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedTrees {
    /// The height of the block.
    pub height: Height,
    /// The Sapling tree.
    pub sapling: Arc<sapling::tree::NoteCommitmentTree>,
    /// The Orchard tree.
    pub orchard: Arc<orchard::tree::NoteCommitmentTree>,
    /// The Ironwood tree.
    pub ironwood: Arc<orchard::tree::NoteCommitmentTree>,
    /// The completed Sapling subtrees, in index order.
    pub sapling_subtrees: Vec<NoteCommitmentSubtree<sapling_crypto::Node>>,
    /// The completed Orchard subtrees, in index order.
    pub orchard_subtrees: Vec<NoteCommitmentSubtree<orchard::tree::Node>>,
    /// The completed Ironwood subtrees, in index order.
    pub ironwood_subtrees: Vec<NoteCommitmentSubtree<orchard::tree::Node>>,
}

impl EmbeddedTrees {
    /// The format version of [`Self::to_bytes`].
    const FORMAT_VERSION: u8 = 1;

    /// Returns the trees embedded for `network`, or `None` if there are none.
    pub fn for_network(network: &Network) -> Option<Arc<Self>> {
        match network {
            Network::Mainnet => Some(Arc::new(
                Self::from_bytes(MAINNET_TREES).expect("embedded trees are valid"),
            )),
            Network::Testnet(_) => None,
        }
    }

    /// Checks that these trees have `roots` and the sizes in `sizes`, and that the subtrees are
    /// the ones these trees completed.
    pub(crate) fn check(&self, roots: &TreeRoots, sizes: [u64; 3]) -> Result<(), TreeRootsError> {
        let error = TreeRootsError::EmbeddedTrees {
            height: self.height,
        };

        // The cached roots are part of the embedded data and get stored, so check both them and
        // the frontiers.
        let matches = self.sapling.recalculate_root() == roots.sapling
            && self.orchard.recalculate_root() == roots.orchard
            && self.ironwood.recalculate_root() == roots.ironwood
            && self.sapling.root() == roots.sapling
            && self.orchard.root() == roots.orchard
            && self.ironwood.root() == roots.ironwood
            && [
                self.sapling.count(),
                self.orchard.count(),
                self.ironwood.count(),
            ] == sizes
            && subtrees_are_complete(&self.sapling_subtrees, self.sapling.count())
            && subtrees_are_complete(&self.orchard_subtrees, self.orchard.count())
            && subtrees_are_complete(&self.ironwood_subtrees, self.ironwood.count());

        matches.then_some(()).ok_or(error)
    }

    /// Serializes these trees.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![Self::FORMAT_VERSION];
        bytes.extend_from_slice(&self.height.0.to_be_bytes());

        for tree in [
            self.sapling.as_bytes(),
            self.orchard.as_bytes(),
            self.ironwood.as_bytes(),
        ] {
            write_section(&mut bytes, &tree);
        }

        write_subtrees(&mut bytes, &self.sapling_subtrees);
        write_subtrees(&mut bytes, &self.orchard_subtrees);
        write_subtrees(&mut bytes, &self.ironwood_subtrees);

        bytes
    }

    /// Deserializes trees serialized by [`Self::to_bytes`].
    pub fn from_bytes(mut bytes: &[u8]) -> io::Result<Self> {
        let reader = &mut bytes;
        let version = read_array::<1>(reader)?[0];
        if version != Self::FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported embedded trees format version {version}"),
            ));
        }

        let height = Height(u32::from_be_bytes(read_array(reader)?));
        let sapling = Arc::new(sapling::tree::NoteCommitmentTree::from_bytes(read_section(
            reader,
        )?));
        let orchard = Arc::new(orchard::tree::NoteCommitmentTree::from_bytes(read_section(
            reader,
        )?));
        let ironwood = Arc::new(orchard::tree::NoteCommitmentTree::from_bytes(read_section(
            reader,
        )?));
        let sapling_subtrees = read_subtrees(reader)?;
        let orchard_subtrees = read_subtrees(reader)?;
        let ironwood_subtrees = read_subtrees(reader)?;

        if !reader.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing bytes after the embedded trees",
            ));
        }

        Ok(Self {
            height,
            sapling,
            orchard,
            ironwood,
            sapling_subtrees,
            orchard_subtrees,
            ironwood_subtrees,
        })
    }
}

/// Returns `true` if `subtrees` are the subtrees a tree with `size` leaves completed: one for
/// each full 2^16 leaves, indexed in order.
fn subtrees_are_complete<Node>(subtrees: &[NoteCommitmentSubtree<Node>], size: u64) -> bool {
    let completed = size >> zebra_chain::subtree::TRACKED_SUBTREE_HEIGHT;

    u64::try_from(subtrees.len()) == Ok(completed)
        && subtrees
            .iter()
            .enumerate()
            .all(|(index, subtree)| usize::from(subtree.index.0) == index)
}

/// Appends a length-prefixed section to `bytes`.
fn write_section(bytes: &mut Vec<u8>, section: &[u8]) {
    let len = u32::try_from(section.len()).expect("embedded sections are small");
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(section);
}

/// Appends `subtrees` to `bytes`, as their count, then each index and the disk format of its data.
fn write_subtrees<Node>(bytes: &mut Vec<u8>, subtrees: &[NoteCommitmentSubtree<Node>])
where
    Node: IntoDisk<Bytes = Vec<u8>> + Clone,
{
    let count = u32::try_from(subtrees.len()).expect("there are fewer than 2^16 subtrees");
    bytes.extend_from_slice(&count.to_be_bytes());

    for subtree in subtrees {
        bytes.extend_from_slice(&subtree.index.as_bytes());
        write_section(
            bytes,
            &NoteCommitmentSubtreeData::new(subtree.end_height, subtree.root.clone()).as_bytes(),
        );
    }
}

/// Reads `N` bytes.
fn read_array<const N: usize>(reader: &mut &[u8]) -> io::Result<[u8; N]> {
    let mut array = [0; N];
    reader.read_exact(&mut array)?;
    Ok(array)
}

/// Reads a section written by [`write_section`].
fn read_section<'a>(reader: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let len = usize::try_from(u32::from_be_bytes(read_array(reader)?))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if reader.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }

    let (section, rest) = reader.split_at(len);
    *reader = rest;
    Ok(section)
}

/// Reads subtrees written by [`write_subtrees`].
fn read_subtrees<Node: FromDisk>(
    reader: &mut &[u8],
) -> io::Result<Vec<NoteCommitmentSubtree<Node>>> {
    let count = u32::from_be_bytes(read_array(reader)?);

    (0..count)
        .map(|_| {
            let index = NoteCommitmentSubtreeIndex::from_bytes(read_array::<2>(reader)?);
            let data = NoteCommitmentSubtreeData::<Node>::from_bytes(read_section(reader)?);
            Ok(data.with_index(index))
        })
        .collect()
}
