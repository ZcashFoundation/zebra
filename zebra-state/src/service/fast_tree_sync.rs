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
    ops::RangeInclusive,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use zebra_chain::{
    block::{self, Height},
    history_tree::{HistoryTree, HistoryTreeError},
    orchard,
    parallel::tree::{NoteCommitmentTrees, TreeRoots},
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

/// Peer-supplied, unverified note commitment tree roots, shared by the state service, which
/// stores them, and the block writer, which verifies and uses them.
#[derive(Debug)]
pub(crate) struct TreeRootsCache {
    /// The cache contents.
    state: Mutex<CacheState>,
    /// Notifies the writer about supplied roots.
    supplied: Condvar,
}

/// The contents of a [`TreeRootsCache`].
#[derive(Debug)]
struct CacheState {
    /// Unverified roots by the height of their block.
    roots: BTreeMap<Height, TreeRoots>,
    /// The height of the next block the writer commits.
    next_height: Height,
    /// The trees fast sync ends with, or `None` if fast sync is unavailable or finished.
    trees: Option<Arc<EmbeddedTrees>>,
    /// The first height fast sync can use roots for.
    first_height: Height,
}

impl TreeRootsCache {
    /// Returns a cache for a writer whose next block is at `next_height`, which ends fast sync at
    /// `trees`.
    pub(crate) fn new(trees: Option<Arc<EmbeddedTrees>>, next_height: Height) -> Self {
        let trees = trees.filter(|trees| trees.height >= next_height);

        Self {
            state: Mutex::new(CacheState {
                roots: BTreeMap::new(),
                next_height,
                trees,
                first_height: Height(0),
            }),
            supplied: Condvar::new(),
        }
    }

    /// Returns the trees fast sync ends with, or `None` if it is unavailable or finished.
    pub(crate) fn embedded_trees(&self) -> Option<Arc<EmbeddedTrees>> {
        self.lock().trees.clone()
    }

    /// Stores the roots after each block from `start_height` that fast sync still needs.
    pub(crate) fn supply(&self, start_height: Height, roots: Vec<TreeRoots>) {
        let mut state = self.lock();
        let Some(window) = state.window() else {
            return;
        };

        for (height, roots) in (start_height.0..).map(Height).zip(roots) {
            if height > *window.end() {
                break;
            }
            if window.contains(&height) {
                state.roots.insert(height, roots);
            }
        }

        self.supplied.notify_all();
    }

    /// Returns the heights fast sync needs roots for first, or `None` if it needs none.
    pub(crate) fn wanted(&self) -> Option<RangeInclusive<Height>> {
        let state = self.lock();
        let window = state.window()?;

        let first_missing = (window.start().0..=window.end().0)
            .map(Height)
            .find(|height| !state.roots.contains_key(height))?;
        let last_missing = state
            .roots
            .range(first_missing..=*window.end())
            .next()
            .map_or(*window.end(), |(height, _)| {
                height
                    .previous()
                    .expect("heights above a missing height are positive")
            });

        Some(first_missing..=last_missing)
    }

    /// Records that the writer will commit `next_height` next, and drops the roots below it.
    pub(crate) fn set_next_height(&self, next_height: Height) {
        let mut state = self.lock();
        state.next_height = next_height;
        state.roots = state.roots.split_off(&next_height);

        if state
            .trees
            .as_ref()
            .is_some_and(|trees| trees.height < next_height)
        {
            state.trees = None;
            state.roots.clear();
        }
    }

    /// Waits up to `timeout` for the supplied roots after the block at `height`.
    pub(crate) fn wait_for_roots(&self, height: Height, timeout: Duration) -> Option<TreeRoots> {
        let state = self.lock();
        let (state, _timeout) = self
            .supplied
            .wait_timeout_while(state, timeout, |state| !state.roots.contains_key(&height))
            .expect("the tree roots cache lock is never poisoned");

        state.roots.get(&height).copied()
    }

    /// Returns the supplied roots after the block at `height`, if there are any.
    pub(crate) fn roots(&self, height: Height) -> Option<TreeRoots> {
        self.lock().roots.get(&height).copied()
    }

    /// Drops the roots after the block at `height`, which failed verification, and the roots after
    /// it, which usually came in the same response.
    pub(crate) fn reject(&self, height: Height) {
        self.lock()
            .roots
            .retain(|roots_height, _| *roots_height < height);
    }

    /// Makes the cache ignore roots below `first_height`, where fast sync can't use them.
    pub(crate) fn with_first_height(self, first_height: Height) -> Self {
        self.lock().first_height = first_height;
        self
    }

    /// Locks the cache contents.
    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state
            .lock()
            .expect("the tree roots cache lock is never poisoned")
    }
}

impl CacheState {
    /// Returns the heights fast sync keeps roots for, or `None` if it needs none.
    fn window(&self) -> Option<RangeInclusive<Height>> {
        let end = self.trees.as_ref()?.height;
        let start = self.next_height.max(self.first_height);
        let window_end = (start + i64::from(TREE_ROOTS_WINDOW - 1))
            .map_or(end, |window_end| window_end.min(end));

        (start <= window_end).then_some(start..=window_end)
    }
}

/// Returns the trees fast sync ends with for a state with `db`, or `None` if it can't fast sync.
///
/// A state that stopped in the middle of fast sync must finish it. Otherwise, fast sync can only
/// start if `enabled`, and only once, so the fast-synced heights stay one range.
pub(crate) fn embedded_trees_for_state(
    network: &Network,
    db: &ZebraDb,
    enabled: bool,
    max_checkpoint_height: Height,
) -> Option<Arc<EmbeddedTrees>> {
    // The block after the embedded trees verifies their block's roots, so it must also be
    // checkpoint-verified.
    let trees =
        EmbeddedTrees::for_network(network).filter(|trees| trees.height < max_checkpoint_height)?;

    match db.fast_sync_tree_heights() {
        Some(heights) if Some(*heights.end()) == db.finalized_tip_height() => Some(trees),
        Some(_) => None,
        None => enabled.then_some(trees),
    }
}

/// How long the block writer waits for missing roots before it logs that it is still waiting.
const ROOTS_WAIT_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// How the block writer commits a checkpoint-verified block.
pub(crate) enum CheckpointCommit {
    /// Commit the block with its verified tree roots, without updating its trees.
    Fast {
        /// The block, as a [`FinalizableBlock::FastCheckpoint`].
        block: Box<FinalizableBlock>,
        /// The tree sizes after the block if the trees stay frozen, or `None` if fast sync ends
        /// with the block.
        frozen_sizes: Option<[u64; 3]>,
    },
    /// Commit the block with tree updates.
    Normal(CheckpointVerifiedBlock),
    /// The supplied roots failed verification while the trees are frozen, so the block can't be
    /// committed now. Either the roots or the next block can be wrong, so both are fetched again.
    Rejected,
    /// The state is shutting down, so the block won't be committed.
    Shutdown,
}

/// The block writer's fast checkpoint sync state.
pub(crate) struct FastTreeSyncWriter {
    /// The peer-supplied roots.
    cache: Arc<TreeRootsCache>,
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
    pub(crate) fn new(db: &ZebraDb, cache: Arc<TreeRootsCache>, network: Network) -> Self {
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
    pub(crate) fn defers(&self, block: &CheckpointVerifiedBlock) -> bool {
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
    /// commitment trees the writer has. While the trees are frozen, waits for roots until
    /// `is_shutting_down` returns `true`.
    pub(crate) fn checkpoint_commit(
        &self,
        block: CheckpointVerifiedBlock,
        next: &CheckpointVerifiedBlock,
        history_tree: &Arc<HistoryTree>,
        trees: &NoteCommitmentTrees,
        is_shutting_down: impl Fn() -> bool,
    ) -> CheckpointCommit {
        let height = block.height;

        loop {
            let roots = if self.frozen_sizes.is_some() {
                match self.cache.wait_for_roots(height, ROOTS_WAIT_LOG_INTERVAL) {
                    Some(roots) => roots,
                    None if is_shutting_down() => return CheckpointCommit::Shutdown,
                    None => {
                        tracing::info!(
                            ?height,
                            "waiting for peers to supply note commitment tree roots"
                        );
                        continue;
                    }
                }
            } else {
                match self.cache.roots(height) {
                    Some(roots) => roots,
                    None => return CheckpointCommit::Normal(block),
                }
            };

            let history_tree =
                match verify_tree_roots(&self.network, &block, &roots, history_tree, next) {
                    Ok(history_tree) => history_tree,
                    Err(error) => {
                        tracing::warn!(?height, %error, "rejected peer-supplied tree roots");
                        self.cache.reject(height);

                        // Until the trees are frozen, updating them is still possible. After
                        // that, the next block's body can be the wrong one, so retry both.
                        return if self.frozen_sizes.is_none() {
                            CheckpointCommit::Normal(block)
                        } else {
                            CheckpointCommit::Rejected
                        };
                    }
                };

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

    /// The embedded trees have different roots or sizes than the chain.
    #[error("the embedded note commitment trees don't match the chain at {height:?}")]
    EmbeddedTrees {
        /// The block height.
        height: Height,
    },
}

/// Verifies `roots`, the supplied roots after `block`, and returns the chain history tree after
/// `block`.
///
/// `history_tree` is the chain history tree before `block`, and `next` is the block after it.
pub(crate) fn verify_tree_roots(
    network: &Network,
    block: &CheckpointVerifiedBlock,
    roots: &TreeRoots,
    history_tree: &Arc<HistoryTree>,
    next: &CheckpointVerifiedBlock,
) -> Result<Arc<HistoryTree>, TreeRootsError> {
    let height = block.height;
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

    check::block_commitment_is_valid_for_chain_history(
        next.block.clone(),
        network,
        &history_tree,
        next.precomputed_auth_data_root(),
    )
    .map_err(commitment_error)?;

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
