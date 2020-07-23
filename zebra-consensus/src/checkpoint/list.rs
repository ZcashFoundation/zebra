//! Checkpoint lists for checkpoint-based block verification
//!
//! Each checkpoint consists of a coinbase height and block header hash.
//!
//! Checkpoints can be used to verify their ancestors, by chaining backwards
//! to another checkpoint, via each block's parent block hash.

use crate::parameters;

use std::{
    collections::{BTreeMap, HashSet},
    error,
    ops::RangeBounds,
    str::FromStr,
};

use zebra_chain::block::BlockHeaderHash;
use zebra_chain::types::BlockHeight;
use zebra_chain::Network::{self, *};

const MAINNET_CHECKPOINTS: &str = include_str!("main-checkpoints.txt");
const TESTNET_CHECKPOINTS: &str = include_str!("test-checkpoints.txt");

/// The inner error type for CheckpointVerifier.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// A list of block height and hash checkpoints.
///
/// Checkpoints should be chosen to avoid forks or chain reorganizations,
/// which only happen in the last few hundred blocks in the chain.
/// (zcashd allows chain reorganizations up to 99 blocks, and prunes
/// orphaned side-chains after 288 blocks.)
///
/// This is actually a bijective map, but since it is read-only, we use a
/// BTreeMap, and do the value uniqueness check on initialisation.
#[derive(Debug)]
pub(crate) struct CheckpointList(BTreeMap<BlockHeight, BlockHeaderHash>);

impl FromStr for CheckpointList {
    type Err = Error;

    /// Parse a string into a CheckpointList.
    ///
    /// Each line has one checkpoint, consisting of a `BlockHeight` and
    /// `BlockHeaderHash`, separated by a single space.
    ///
    /// Assumes that the provided genesis checkpoint is correct.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut checkpoint_list: Vec<(BlockHeight, BlockHeaderHash)> = Vec::new();

        for checkpoint in s.lines() {
            let fields = checkpoint.split(' ').collect::<Vec<_>>();
            if let [height, hash] = fields[..] {
                checkpoint_list.push((height.parse()?, hash.parse()?));
            } else {
                Err(format!("Invalid checkpoint format: expected 2 space-separated fields but found {}: '{}'", fields.len(), checkpoint))?;
            };
        }

        Ok(CheckpointList::from_list(checkpoint_list)?)
    }
}

impl CheckpointList {
    /// Returns the hard-coded checkpoint list for `network`.
    pub fn new(network: Network) -> Self {
        // parse calls CheckpointList::from_list
        let checkpoint_list: CheckpointList = match network {
            Mainnet => MAINNET_CHECKPOINTS
                .parse()
                .expect("Hard-coded Mainnet checkpoint list parses and validates"),
            Testnet => TESTNET_CHECKPOINTS
                .parse()
                .expect("Hard-coded Testnet checkpoint list parses and validates"),
        };

        match checkpoint_list.hash(BlockHeight(0)) {
            Some(hash) if hash == parameters::genesis_hash(network) => checkpoint_list,
            Some(_) => {
                panic!("The hard-coded genesis checkpoint does not match the network genesis hash")
            }
            None => unreachable!("Parser should have checked for a missing genesis checkpoint"),
        }
    }

    /// Create a new checkpoint list for `network` from `checkpoint_list`.
    ///
    /// Assumes that the provided genesis checkpoint is correct.
    ///
    /// Checkpoint heights and checkpoint hashes must be unique.
    /// There must be a checkpoint for a genesis block at BlockHeight 0.
    /// (All other checkpoints are optional.)
    pub(crate) fn from_list(
        list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        // BTreeMap silently ignores duplicates, so we count the checkpoints
        // before adding them to the map
        let original_checkpoints: Vec<(BlockHeight, BlockHeaderHash)> = list.into_iter().collect();
        let original_len = original_checkpoints.len();

        let checkpoints: BTreeMap<BlockHeight, BlockHeaderHash> =
            original_checkpoints.into_iter().collect();

        // Check that the list starts with the correct genesis block
        match checkpoints.iter().next() {
            Some((BlockHeight(0), hash))
                if (hash == &parameters::genesis_hash(Mainnet)
                    || hash == &parameters::genesis_hash(Testnet)) => {}
            Some((BlockHeight(0), _)) => {
                Err("the genesis checkpoint does not match the Mainnet or Testnet genesis hash")?
            }
            Some(_) => Err("checkpoints must start at the genesis block height 0")?,
            None => Err("there must be at least one checkpoint, for the genesis block")?,
        };

        // This check rejects duplicate heights, whether they have the same or
        // different hashes
        if checkpoints.len() != original_len {
            Err("checkpoint heights must be unique")?;
        }

        let block_hashes: HashSet<&BlockHeaderHash> = checkpoints.values().collect();
        if block_hashes.len() != original_len {
            Err("checkpoint hashes must be unique")?;
        }

        // Make sure all the hashes are valid. In Bitcoin, [0; 32] is the null
        // hash. It is also used as the parent hash of genesis blocks.
        if block_hashes.contains(&BlockHeaderHash([0; 32])) {
            Err("checkpoint list contains invalid checkpoint hash: found null hash")?;
        }

        let checkpoints = CheckpointList(checkpoints);
        if checkpoints.max_height() > BlockHeight::MAX {
            Err("checkpoint list contains invalid checkpoint: checkpoint height is greater than the maximum block height")?;
        }

        Ok(checkpoints)
    }

    /// Return true if there is a checkpoint at `height`.
    ///
    /// See `BTreeMap::contains_key()` for details.
    pub fn contains(&self, height: BlockHeight) -> bool {
        self.0.contains_key(&height)
    }

    /// Returns the hash corresponding to the checkpoint at `height`,
    /// or None if there is no checkpoint at that height.
    ///
    /// See `BTreeMap::get()` for details.
    pub fn hash(&self, height: BlockHeight) -> Option<BlockHeaderHash> {
        self.0.get(&height).cloned()
    }

    /// Return the block height of the highest checkpoint in the checkpoint list.
    ///
    /// If there is only a single checkpoint, then the maximum height will be
    /// zero. (The genesis block.)
    pub fn max_height(&self) -> BlockHeight {
        self.max_height_in_range(..)
            .expect("checkpoint lists must have at least one checkpoint")
    }

    /// Return the block height of the highest checkpoint in a sub-range.
    pub fn max_height_in_range<R>(&self, range: R) -> Option<BlockHeight>
    where
        R: RangeBounds<BlockHeight>,
    {
        self.0.range(range).map(|(height, _)| *height).next_back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use zebra_chain::{block::Block, serialization::ZcashDeserialize};

    /// Make a checkpoint list containing only the genesis block
    #[test]
    fn checkpoint_list_genesis() -> Result<(), Error> {
        zebra_test::init();

        // Parse the genesis block
        let mut checkpoint_data = Vec::new();
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));

        // Make a checkpoint list containing the genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::from_list(checkpoint_list)?;

        Ok(())
    }

    /// Make a checkpoint list containing multiple blocks
    #[test]
    fn checkpoint_list_multiple() -> Result<(), Error> {
        zebra_test::init();

        // Parse all the blocks
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Make a checkpoint list containing all the blocks
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::from_list(checkpoint_list)?;

        Ok(())
    }

    /// Make sure that an empty checkpoint list fails
    #[test]
    fn checkpoint_list_empty_fail() -> Result<(), Error> {
        zebra_test::init();

        let _ =
            CheckpointList::from_list(Vec::new()).expect_err("empty checkpoint lists should fail");

        Ok(())
    }

    /// Make sure a checkpoint list that doesn't contain the genesis block fails
    #[test]
    fn checkpoint_list_no_genesis_fail() -> Result<(), Error> {
        zebra_test::init();

        // Parse a non-genesis block
        let mut checkpoint_data = Vec::new();
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::from_list(checkpoint_list)
            .expect_err("a checkpoint list with no genesis block should fail");

        Ok(())
    }

    /// Make sure a checkpoint list that contains a null hash fails
    #[test]
    fn checkpoint_list_null_hash_fail() -> Result<(), Error> {
        zebra_test::init();

        let checkpoint_data = vec![(BlockHeight(0), BlockHeaderHash([0; 32]))];

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::from_list(checkpoint_list)
            .expect_err("a checkpoint list with a null block hash should fail");

        Ok(())
    }

    /// Make sure a checkpoint list that contains an invalid block height fails
    #[test]
    fn checkpoint_list_bad_height_fail() -> Result<(), Error> {
        zebra_test::init();

        let checkpoint_data = vec![(
            BlockHeight(BlockHeight::MAX.0 + 1),
            BlockHeaderHash([1; 32]),
        )];

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::from_list(checkpoint_list).expect_err(
            "a checkpoint list with an invalid block height (BlockHeight::MAX + 1) should fail",
        );

        let checkpoint_data = vec![(BlockHeight(u32::MAX), BlockHeaderHash([1; 32]))];

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::from_list(checkpoint_list)
            .expect_err("a checkpoint list with an invalid block height (u32::MAX) should fail");

        Ok(())
    }

    /// Make sure that a checkpoint list containing duplicate blocks fails
    #[test]
    fn checkpoint_list_duplicate_blocks_fail() -> Result<(), Error> {
        zebra_test::init();

        // Parse some blocks twice
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Make a checkpoint list containing some duplicate blocks
        let _ = CheckpointList::from_list(checkpoint_data)
            .expect_err("checkpoint lists with duplicate blocks should fail");

        Ok(())
    }

    /// Make sure that a checkpoint list containing duplicate heights
    /// (with different hashes) fails
    #[test]
    fn checkpoint_list_duplicate_heights_fail() -> Result<(), Error> {
        zebra_test::init();

        // Parse the genesis block
        let mut checkpoint_data = Vec::new();
        for b in &[&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Then add some fake entries with duplicate heights
        checkpoint_data.push((BlockHeight(1), BlockHeaderHash([0xaa; 32])));
        checkpoint_data.push((BlockHeight(1), BlockHeaderHash([0xbb; 32])));

        // Make a checkpoint list containing some duplicate blocks
        let _ = CheckpointList::from_list(checkpoint_data)
            .expect_err("checkpoint lists with duplicate heights should fail");

        Ok(())
    }

    /// Make sure that a checkpoint list containing duplicate hashes
    /// (at different heights) fails
    #[test]
    fn checkpoint_list_duplicate_hashes_fail() -> Result<(), Error> {
        zebra_test::init();

        // Parse the genesis block
        let mut checkpoint_data = Vec::new();
        for b in &[&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Then add some fake entries with duplicate hashes
        checkpoint_data.push((BlockHeight(1), BlockHeaderHash([0xcc; 32])));
        checkpoint_data.push((BlockHeight(2), BlockHeaderHash([0xcc; 32])));

        // Make a checkpoint list containing some duplicate blocks
        let _ = CheckpointList::from_list(checkpoint_data)
            .expect_err("checkpoint lists with duplicate hashes should fail");

        Ok(())
    }

    /// Parse the hard-coded Mainnet and Testnet lists
    #[test]
    fn checkpoint_list_hard_coded() -> Result<(), Error> {
        zebra_test::init();

        let _: CheckpointList = MAINNET_CHECKPOINTS
            .parse()
            .expect("hard-coded Mainnet checkpoint list should parse");
        let _: CheckpointList = TESTNET_CHECKPOINTS
            .parse()
            .expect("hard-coded Testnet checkpoint list should parse");

        let _ = CheckpointList::new(Mainnet);
        let _ = CheckpointList::new(Testnet);

        Ok(())
    }
}
