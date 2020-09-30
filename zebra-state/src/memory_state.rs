//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html
#![allow(dead_code)]
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    ops::Deref,
    sync::Arc,
};

use zebra_chain::{
    block::{self, Block},
    primitives::Groth16Proof,
    sapling, sprout, transaction, transparent,
    work::difficulty::PartialCumulativeWork,
};

use crate::service::QueuedBlock;

/// The state of the chains in memory, incuding queued blocks.
#[derive(Default)]
pub struct NonFinalizedState {
    /// Verified, non-finalized chains.
    chain_set: BTreeSet<Chain>,
    /// Blocks awaiting their parent blocks for contextual verification.
    contextual_queue: QueuedBlocks,
}

/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Default)]
struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, QueuedBlock>,
    /// Hashes from `queued_blocks`, indexed by parent hash.
    by_parent: HashMap<block::Hash, Vec<block::Hash>>,
    /// Hashes from `queued_blocks`, indexed by block height.
    by_height: BTreeMap<block::Height, Vec<block::Hash>>,
}

impl NonFinalizedState {
    pub fn finalize(&mut self) -> Arc<Block> {
        todo!()
    }

    pub fn queue(&mut self, _block: QueuedBlock) {
        todo!()
    }

    fn process_queued(&mut self, _new_parent: block::Hash) {
        todo!()
    }

    fn commit_block(&mut self, _block: QueuedBlock) -> Option<block::Hash> {
        todo!()
    }
}

#[derive(Default, Clone)]
struct Chain {
    blocks: BTreeMap<block::Height, Arc<Block>>,
    height_by_hash: HashMap<block::Hash, block::Height>,
    tx_by_hash: HashMap<transaction::Hash, (block::Height, usize)>,

    created_utxos: HashSet<transparent::OutPoint>,
    spent_utxos: HashSet<transparent::OutPoint>,
    sprout_anchors: HashSet<sprout::tree::Root>,
    sapling_anchors: HashSet<sapling::tree::Root>,
    sprout_nullifiers: HashSet<sprout::Nullifier>,
    sapling_nullifiers: HashSet<sapling::Nullifier>,
    partial_cumulative_work: PartialCumulativeWork,
}

impl Chain {
    /// Push a contextually valid non-finalized block into a chain as the new tip.
    pub fn push(&mut self, block: Arc<Block>) {
        let block_height = block
            .coinbase_height()
            .expect("valid non-finalized blocks have a coinbase height");

        // update cumulative data members
        self.update_chain_state_with(&block);
        self.blocks.insert(block_height, block);
    }

    /// Remove the lowest height block of the non-finalized portion of a chain.
    pub fn pop_root(&mut self) -> Arc<Block> {
        let block_height = self.lowest_height();

        // remove the lowest height block from self.blocks
        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while blocks is populated");

        // update cumulative data members
        self.revert_chain_state_with(&block);

        // return the block
        block
    }

    fn lowest_height(&self) -> block::Height {
        self.blocks
            .keys()
            .next()
            .cloned()
            .expect("only called while blocks is populated")
    }

    /// Fork a chain at the block with the givin hash, if it is part of this
    /// chain.
    pub fn fork(&self, fork_tip: block::Hash) -> Option<Self> {
        if !self.height_by_hash.contains_key(&fork_tip) {
            return None;
        }

        let mut forked = self.clone();

        while forked.non_finalized_tip_hash() != fork_tip {
            forked.pop_tip();
        }

        Some(forked)
    }

    fn non_finalized_tip_hash(&self) -> block::Hash {
        self.blocks
            .values()
            .next_back()
            .expect("only called while blocks is populated")
            .hash()
    }

    /// Remove the highest height block of the non-finalized portion of a chain.
    fn pop_tip(&mut self) {
        let block_height = self.non_finalized_tip_height();

        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while blocks is populated");

        assert!(
            !self.blocks.is_empty(),
            "Non-finalized chains must have at least one block to be valid"
        );

        self.revert_chain_state_with(&block);
    }

    fn non_finalized_tip_height(&self) -> block::Height {
        *self
            .blocks
            .keys()
            .next_back()
            .expect("only called while blocks is populated")
    }
}

/// Helper trait to organize inverse operations done on the `Chain` type. Used to
/// overload the `update_chain_state_with` and `revert_chain_state_with` methods
/// based on the type of the argument.
///
/// This trait was motivated by the length of the `push` and `pop_root` functions
/// and fear that it would be easy to introduce bugs when updating them unless
/// the code was reorganized to keep related operations adjacent to eachother.
trait UpdateWith<T> {
    /// Update `Chain` cumulative data members to add data that are derived from
    /// `T`
    fn update_chain_state_with(&mut self, _: &T);

    /// Update `Chain` cumulative data members to remove data that are derived
    /// from `T`
    fn revert_chain_state_with(&mut self, _: &T);
}

impl UpdateWith<Arc<Block>> for Chain {
    fn update_chain_state_with(&mut self, block: &Arc<Block>) {
        let block_height = block
            .coinbase_height()
            .expect("valid non-finalized blocks have a coinbase height");
        let block_hash = block.hash();

        // add hash to height_by_hash
        let prior_height = self.height_by_hash.insert(block_hash, block_height);
        assert!(
            prior_height.is_none(),
            "block heights must be unique within a single chain"
        );

        // add work to partial cumulative work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work += block_work;

        // for each transaction in block
        for (transaction_index, transaction) in block.transactions.iter().enumerate() {
            let (inputs, outputs, shielded_data, joinsplit_data) = match transaction.deref() {
                transaction::Transaction::V4 {
                    inputs,
                    outputs,
                    shielded_data,
                    joinsplit_data,
                    ..
                } => (inputs, outputs, shielded_data, joinsplit_data),
                _ => unreachable!(
                    "older transaction versions only exist in finalized blocks pre sapling",
                ),
            };

            // add key `transaction.hash` and value `(height, tx_index)` to `tx_by_hash`
            let transaction_hash = transaction.hash();
            let prior_pair = self
                .tx_by_hash
                .insert(transaction_hash, (block_height, transaction_index));
            assert!(
                prior_pair.is_none(),
                "transactions must be unique within a single chain"
            );

            // add the utxos this produced
            self.update_chain_state_with(&(transaction_hash, outputs));
            // add the utxos this consumed
            self.update_chain_state_with(inputs);
            // add sprout anchor and nullifiers
            self.update_chain_state_with(joinsplit_data);
            // add sapling anchor and nullifier
            self.update_chain_state_with(shielded_data);
        }
    }

    fn revert_chain_state_with(&mut self, block: &Arc<Block>) {
        let block_hash = block.hash();

        // remove the blocks hash from `height_by_hash`
        assert!(
            self.height_by_hash.remove(&block_hash).is_some(),
            "hash must be present if block was"
        );

        // remove work from partial_cumulative_work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work -= block_work;

        // for each transaction in block
        for transaction in &block.transactions {
            let (inputs, outputs, shielded_data, joinsplit_data) = match transaction.deref() {
                transaction::Transaction::V4 {
                    inputs,
                    outputs,
                    shielded_data,
                    joinsplit_data,
                    ..
                } => (inputs, outputs, shielded_data, joinsplit_data),
                _ => unreachable!(
                    "older transaction versions only exist in finalized blocks pre sapling",
                ),
            };

            // remove `transaction.hash` from `tx_by_hash`
            let transaction_hash = transaction.hash();
            assert!(
                self.tx_by_hash.remove(&transaction_hash).is_some(),
                "transactions must be present if block was"
            );

            // remove the utxos this produced
            self.revert_chain_state_with(&(transaction_hash, outputs));
            // remove the utxos this consumed
            self.revert_chain_state_with(inputs);
            // remove sprout anchor and nullifiers
            self.revert_chain_state_with(joinsplit_data);
            // remove sapling anchor and nullfier
            self.revert_chain_state_with(shielded_data);
        }
    }
}

impl UpdateWith<(transaction::Hash, &Vec<transparent::Output>)> for Chain {
    fn update_chain_state_with(
        &mut self,
        (transaction_hash, outputs): &(transaction::Hash, &Vec<transparent::Output>),
    ) {
        for (utxo_index, _) in outputs.iter().enumerate() {
            self.created_utxos.insert(transparent::OutPoint {
                hash: *transaction_hash,
                index: utxo_index as u32,
            });
        }
    }

    fn revert_chain_state_with(
        &mut self,
        (transaction_hash, outputs): &(transaction::Hash, &Vec<transparent::Output>),
    ) {
        for (utxo_index, _) in outputs.iter().enumerate() {
            assert!(
                self.created_utxos.remove(&transparent::OutPoint {
                    hash: *transaction_hash,
                    index: utxo_index as u32,
                }),
                "created_utxos must be present if block was"
            );
        }
    }
}

impl UpdateWith<Vec<transparent::Input>> for Chain {
    fn update_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut { outpoint, .. } => {
                    self.spent_utxos.insert(*outpoint);
                }
                transparent::Input::Coinbase { .. } => {}
            }
        }
    }

    fn revert_chain_state_with(&mut self, inputs: &Vec<transparent::Input>) {
        for consumed_utxo in inputs {
            match consumed_utxo {
                transparent::Input::PrevOut { outpoint, .. } => {
                    assert!(
                        self.spent_utxos.remove(outpoint),
                        "spent_utxos must be present if block was"
                    );
                }
                transparent::Input::Coinbase { .. } => {}
            }
        }
    }
}

impl UpdateWith<Option<transaction::JoinSplitData<Groth16Proof>>> for Chain {
    fn update_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            for sprout::JoinSplit {
                anchor, nullifiers, ..
            } in joinsplit_data.joinsplits()
            {
                self.sprout_anchors.insert(*anchor);
                self.sprout_nullifiers.insert(nullifiers[0]);
                self.sprout_nullifiers.insert(nullifiers[1]);
            }
        }
    }

    fn revert_chain_state_with(
        &mut self,
        joinsplit_data: &Option<transaction::JoinSplitData<Groth16Proof>>,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            for sprout::JoinSplit {
                anchor, nullifiers, ..
            } in joinsplit_data.joinsplits()
            {
                assert!(
                    self.sprout_anchors.remove(anchor),
                    "anchor must be present if block was"
                );
                assert!(
                    self.sprout_nullifiers.remove(&nullifiers[0]),
                    "nullifiers must be present if block was"
                );
                assert!(
                    self.sprout_nullifiers.remove(&nullifiers[1]),
                    "nullifiers must be present if block was"
                );
            }
        }
    }
}

impl UpdateWith<Option<transaction::ShieldedData>> for Chain {
    fn update_chain_state_with(&mut self, shielded_data: &Option<transaction::ShieldedData>) {
        if let Some(shielded_data) = shielded_data {
            for sapling::Spend {
                anchor, nullifier, ..
            } in shielded_data.spends()
            {
                self.sapling_anchors.insert(*anchor);
                self.sapling_nullifiers.insert(*nullifier);
            }
        }
    }

    fn revert_chain_state_with(&mut self, shielded_data: &Option<transaction::ShieldedData>) {
        if let Some(shielded_data) = shielded_data {
            for sapling::Spend {
                anchor, nullifier, ..
            } in shielded_data.spends()
            {
                assert!(
                    self.sapling_anchors.remove(anchor),
                    "anchor must be present if block was"
                );
                assert!(
                    self.sapling_nullifiers.remove(nullifier),
                    "nullifier must be present if block was"
                );
            }
        }
    }
}

impl PartialEq for Chain {
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

impl Eq for Chain {}

impl PartialOrd for Chain {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Chain {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.partial_cumulative_work != other.partial_cumulative_work {
            self.partial_cumulative_work
                .cmp(&other.partial_cumulative_work)
        } else {
            let self_hash = self
                .blocks
                .values()
                .last()
                .expect("always at least 1 element")
                .hash();

            let other_hash = other
                .blocks
                .values()
                .last()
                .expect("always at least 1 element")
                .hash();

            // This comparison is a tie-breaker within the local node, so it does not need to
            // be consistent with the ordering on `ExpandedDifficulty` and `block::Hash`.
            match self_hash.0.cmp(&other_hash.0) {
                Ordering::Equal => unreachable!("Chain tip block hashes are always unique"),
                ordering => ordering,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use transaction::Transaction;

    use std::{env, mem};

    use zebra_chain::serialization::ZcashDeserializeInto;
    use zebra_chain::{
        parameters::{Network, NetworkUpgrade},
        LedgerState,
    };
    use zebra_test::prelude::*;

    use self::assert_eq;
    use super::*;

    /// Helper trait for constructing "valid" looking chains of blocks
    trait FakeChainHelper {
        fn make_fake_child(&self) -> Arc<Block>;
    }

    impl FakeChainHelper for Block {
        fn make_fake_child(&self) -> Arc<Block> {
            let parent_hash = self.hash();
            let mut child = Block::clone(self);
            let mut transactions = mem::take(&mut child.transactions);
            let mut tx = transactions.remove(0);

            let input = match Arc::make_mut(&mut tx) {
                Transaction::V1 { inputs, .. } => &mut inputs[0],
                Transaction::V2 { inputs, .. } => &mut inputs[0],
                Transaction::V3 { inputs, .. } => &mut inputs[0],
                Transaction::V4 { inputs, .. } => &mut inputs[0],
            };

            match input {
                transparent::Input::Coinbase { height, .. } => height.0 += 1,
                _ => panic!("block must have a coinbase height to create a child"),
            }

            child.transactions.push(tx);
            child.header.previous_block_hash = parent_hash;

            Arc::new(child)
        }
    }

    #[test]
    fn construct_empty() {
        zebra_test::init();
        let _chain = Chain::default();
    }

    #[test]
    fn construct_single() -> Result<()> {
        zebra_test::init();
        let block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;

        let mut chain = Chain::default();
        chain.push(block);

        assert_eq!(1, chain.blocks.len());

        Ok(())
    }

    #[test]
    fn construct_many() -> Result<()> {
        zebra_test::init();

        let mut block: Arc<Block> =
            zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
        let mut blocks = vec![];

        while blocks.len() < 100 {
            let next_block = block.make_fake_child();
            blocks.push(block);
            block = next_block;
        }

        let mut chain = Chain::default();

        for block in blocks {
            chain.push(block);
        }

        assert_eq!(100, chain.blocks.len());

        Ok(())
    }

    fn arbitrary_chain(height: block::Height) -> BoxedStrategy<Vec<Arc<Block>>> {
        Block::partial_chain_strategy(
            LedgerState {
                tip_height: height,
                is_coinbase: true,
                network: Network::Mainnet,
            },
            100,
        )
    }

    prop_compose! {
        fn arbitrary_chain_and_count()
            (chain in arbitrary_chain(NetworkUpgrade::Blossom.activation_height(Network::Mainnet).unwrap()))
            (count in 1..chain.len(), chain in Just(chain)) -> (NoDebug<Vec<Arc<Block>>>, usize)
        {
            (NoDebug(chain), count)
        }
    }

    #[test]
    fn forked_equals_pushed() -> Result<()> {
        zebra_test::init();

        proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(1)),
        |((chain, count) in arbitrary_chain_and_count())| {
            let chain = chain.0;
            let fork_tip_hash = chain[count - 1].hash();
            let mut full_chain = Chain::default();
            let mut partial_chain = Chain::default();

            for block in chain.iter().take(count) {
                partial_chain.push(block.clone());
            }

            for block in chain {
                full_chain.push(block);
            }

            let forked = full_chain.fork(fork_tip_hash).expect("hash is present");

            prop_assert_eq!(forked.blocks.len(), partial_chain.blocks.len());

        });

        Ok(())
    }

    #[test]
    fn finalized_equals_pushed() -> Result<()> {
        zebra_test::init();

        proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(1)),
        |((chain, end_count) in arbitrary_chain_and_count())| {
            let chain = chain.0;
            let finalized_count = chain.len() - end_count;
            let mut full_chain = Chain::default();
            let mut partial_chain = Chain::default();

            for block in chain.iter().skip(finalized_count) {
                partial_chain.push(block.clone());
            }

            for block in chain {
                full_chain.push(block);
            }

            for _ in 0..finalized_count {
                let _finalized = full_chain.pop_root();
            }

            prop_assert_eq!(full_chain.blocks.len(), partial_chain.blocks.len());

        });

        Ok(())
    }
}

struct NoDebug<T>(T);

impl<T> fmt::Debug for NoDebug<Vec<T>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, len={}", std::any::type_name::<T>(), self.0.len())
    }
}
