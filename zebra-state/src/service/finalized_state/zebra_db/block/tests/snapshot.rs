//! Database snapshot tests for blocks and transactions.
//!
//! These tests check the values returned by all the APIs in the `zebra_db::block` module,
//! iterating through the possible arguments as needed:
//! - tip
//! - hash(height: tip..=0)
//! - height(hash: heights)
//! - block(height: tip..=0)
//! - transaction(hash: blocks.transactions.hashes)
//!
//! But skips the following trivial variations:
//! - is_empty: covered by `tip == None`
//! - block(hash): covered by `height(hash) and block(height)`
//!
//! These tests use fixed test vectors, based on the results of other database queries.
//!
//! # Snapshot Format
//!
//! These snapshots use [RON (Rusty Object Notation)](https://github.com/ron-rs/ron#readme),
//! a text format similar to Rust syntax. Raw byte data is encoded in hexadecimal.
//!
//! Due to `serde` limitations, some object types can't be represented exactly,
//! so RON uses the closest equivalent structure.
//!
//! # Fixing Test Failures
//!
//! If this test fails, run:
//! ```sh
//! cargo insta test --review --delete-unreferenced-snapshots
//! ```
//! to update the test snapshots, then commit the `test_*.snap` files using git.

use std::sync::Arc;

use serde::Serialize;

use zebra_chain::{
    block::{self, Block, Height, SerializedBlock},
    orchard,
    parameters::Network::{self, *},
    sapling,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{self, Transaction},
    transparent,
};

use crate::{
    service::finalized_state::{
        disk_format::{block::TransactionIndex, transparent::OutputLocation, TransactionLocation},
        FinalizedState,
    },
    Config,
};

/// Tip structure for RON snapshots.
///
/// This structure snapshots the height and hash on separate lines,
/// which looks good for a single entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Tip {
    height: u32,
    block_hash: String,
}

impl From<(Height, block::Hash)> for Tip {
    fn from((height, hash): (Height, block::Hash)) -> Tip {
        Tip {
            height: height.0,
            block_hash: hash.to_string(),
        }
    }
}

/// Block hash structure for RON snapshots.
///
/// This structure is used to snapshot the height and hash on the same line,
/// which looks good for a vector of heights and hashes.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
struct BlockHash(String);

/// Block data structure for RON snapshots.
///
/// This structure is used to snapshot the height and block data on separate lines,
/// which looks good for a vector of heights and block data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BlockData {
    height: u32,
    #[serde(with = "hex")]
    block: SerializedBlock,
}

impl BlockData {
    pub fn new(height: Height, block: &Block) -> BlockData {
        BlockData {
            height: height.0,
            block: block.into(),
        }
    }
}

/// Transaction hash structure for RON snapshots.
///
/// This structure is used to snapshot the location and transaction hash on separate lines,
/// which looks good for a vector of locations and transaction hashes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TransactionHashByLocation {
    loc: Option<TransactionLocation>,
    #[serde(with = "hex")]
    hash: transaction::Hash,
}

impl TransactionHashByLocation {
    pub fn new(
        loc: Option<TransactionLocation>,
        hash: transaction::Hash,
    ) -> TransactionHashByLocation {
        TransactionHashByLocation { loc, hash }
    }
}

/// Transaction data structure for RON snapshots.
///
/// This structure is used to snapshot the location and transaction data on separate lines,
/// which looks good for a vector of locations and transaction data.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
struct TransactionData {
    loc: TransactionLocation,
    // TODO: after #3145, replace with:
    // #[serde(with = "hex")]
    // transaction: SerializedTransaction,
    transaction: String,
}

impl TransactionData {
    pub fn new(loc: TransactionLocation, transaction: &Transaction) -> TransactionData {
        let transaction = transaction
            .zcash_serialize_to_vec()
            .expect("serialization of stored transaction succeeds");

        TransactionData {
            loc,
            transaction: hex::encode(transaction),
        }
    }
}

/// Snapshot test for finalized block and transaction data.
#[test]
fn test_block_and_transaction_data() {
    zebra_test::init();

    test_block_and_transaction_data_with_network(Mainnet);
    test_block_and_transaction_data_with_network(Testnet);
}

/// Snapshot finalized block and transaction data for `network`.
///
/// See [`test_block_and_transaction_data`].
fn test_block_and_transaction_data_with_network(network: Network) {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();

    let mut state = FinalizedState::new(&Config::ephemeral(), network);

    // Assert that empty databases are the same, regardless of the network.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix("no_blocks");

    settings.bind(|| snapshot_block_and_transaction_data(&state));

    // Snapshot block and transaction database data for:
    // - mainnet and testnet
    // - genesis, block 1, and block 2
    let blocks = match network {
        Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
    };

    // We limit the number of blocks, because the serialized data is a few kilobytes per block.
    //
    // TODO: Test data activated in Overwinter and later network upgrades.
    for height in 0..=2 {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        state
            .commit_finalized_direct(block.into(), "snapshot tests")
            .expect("test block is valid");

        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!("{}_{}", net_suffix, height));

        settings.bind(|| snapshot_block_and_transaction_data(&state));
    }
}

/// Snapshot block and transaction data, using `cargo insta` and RON serialization.
fn snapshot_block_and_transaction_data(state: &FinalizedState) {
    let tip = state.tip();

    insta::assert_ron_snapshot!("tip", tip.map(Tip::from));

    if let Some((max_height, tip_block_hash)) = tip {
        // Check that the database returns empty note commitment trees for the
        // genesis block.
        let sapling_tree = state
            .sapling_note_commitment_tree_by_height(&block::Height::MIN)
            .expect("the genesis block in the database has a Sapling tree");
        let orchard_tree = state
            .orchard_note_commitment_tree_by_height(&block::Height::MIN)
            .expect("the genesis block in the database has an Orchard tree");

        assert_eq!(sapling_tree, sapling::tree::NoteCommitmentTree::default());
        assert_eq!(orchard_tree, orchard::tree::NoteCommitmentTree::default());

        let mut stored_block_hashes = Vec::new();
        let mut stored_blocks = Vec::new();

        let mut stored_sapling_trees = Vec::new();
        let mut stored_orchard_trees = Vec::new();

        let mut stored_transaction_hashes = Vec::new();
        let mut stored_transactions = Vec::new();

        let mut stored_utxos = Vec::new();

        let sapling_tree_at_tip = state.sapling_note_commitment_tree();
        let orchard_tree_at_tip = state.orchard_note_commitment_tree();

        // Test the history tree.
        //
        // TODO: test non-empty history trees, using Heartwood or later blocks.
        //       test the rest of the chain data (value balance).
        let history_tree_at_tip = state.history_tree();

        // TODO: split out block snapshots into their own function (#3151)
        for query_height in 0..=max_height.0 {
            let query_height = Height(query_height);

            // Check all the block column families,
            // using block height, block hash, and block database queries.
            let stored_block_hash = state
                .hash(query_height)
                .expect("heights up to tip have hashes");
            let stored_height = state
                .height(stored_block_hash)
                .expect("hashes up to tip have heights");
            let stored_block = state
                .block(query_height.into())
                .expect("heights up to tip have blocks");

            // Check the sapling and orchard note commitment trees.
            //
            // TODO: test the rest of the shielded data (anchors, nullifiers, sprout)
            let sapling_tree_by_height = state
                .sapling_note_commitment_tree_by_height(&query_height)
                .expect("heights up to tip have Sapling trees");
            let orchard_tree_by_height = state
                .orchard_note_commitment_tree_by_height(&query_height)
                .expect("heights up to tip have Orchard trees");

            // We don't need to snapshot the heights,
            // because they are fully determined by the tip and block hashes.
            //
            // But we do it anyway, so the snapshots are more readable.

            // Check that the heights are consistent.
            assert_eq!(stored_height, query_height);

            assert_eq!(
                stored_block
                    .coinbase_height()
                    .expect("stored blocks have valid heights"),
                query_height,
            );

            // Check that the tips are consistent.
            if query_height == max_height {
                assert_eq!(stored_block_hash, tip_block_hash);

                assert_eq!(sapling_tree_at_tip, sapling_tree_by_height);
                assert_eq!(orchard_tree_at_tip, orchard_tree_by_height);

                // Skip these checks for empty history trees.
                if let Some(history_tree_at_tip) = history_tree_at_tip.as_ref() {
                    assert_eq!(history_tree_at_tip.current_height(), max_height);
                    assert_eq!(history_tree_at_tip.network(), state.network());
                }
            }

            stored_block_hashes.push((stored_height, BlockHash(stored_block_hash.to_string())));
            stored_blocks.push(BlockData::new(stored_height, &stored_block));

            stored_sapling_trees.push((stored_height, sapling_tree_by_height));
            stored_orchard_trees.push((stored_height, orchard_tree_by_height));

            // Check block transaction hashes and transactions.
            //
            // TODO: split out transaction snapshots into their own function (#3151)
            for tx_index in 0..stored_block.transactions.len() {
                let block_transaction = &stored_block.transactions[tx_index];
                let transaction_location = TransactionLocation::from_usize(query_height, tx_index);

                let transaction_hash = block_transaction.hash();
                let transaction_data =
                    TransactionData::new(transaction_location, block_transaction);

                // Check all the transaction column families,
                // using transaction location queries.

                // Check that the transaction indexes are consistent.
                let (direct_transaction, direct_transaction_height) = state
                    .transaction(transaction_hash)
                    .expect("transactions in blocks must also be available directly");
                let stored_transaction_hash = state
                    .transaction_hash(transaction_location)
                    .expect("hashes of transactions in blocks must be indexed by location");
                let stored_transaction_location = state
                    .transaction_location(transaction_hash)
                    .expect("locations of transactions in blocks must be indexed by hash");

                assert_eq!(
                    &direct_transaction, block_transaction,
                    "transactions in block must be the same as transactions looked up directly",
                );
                assert_eq!(
                    direct_transaction_height, transaction_location.height,
                    "transaction heights must be the same as their block heights",
                );
                assert_eq!(stored_transaction_hash, transaction_hash);
                assert_eq!(stored_transaction_location, transaction_location);

                // TODO: snapshot TransactionLocations without Some (#3151)
                let stored_transaction_hash = TransactionHashByLocation::new(
                    Some(stored_transaction_location),
                    transaction_hash,
                );

                stored_transaction_hashes.push(stored_transaction_hash);
                stored_transactions.push(transaction_data);

                for output_index in 0..stored_block.transactions[tx_index].outputs().len() {
                    let output = &stored_block.transactions[tx_index].outputs()[output_index];
                    let outpoint =
                        transparent::OutPoint::from_usize(transaction_hash, output_index);

                    let output_location =
                        OutputLocation::from_usize(transaction_hash, output_index);

                    let stored_utxo = state.utxo(&outpoint);

                    // # Consensus
                    //
                    // The genesis transaction's UTXO is not indexed.
                    // This check also ignores spent UTXOs.
                    if let Some(stored_utxo) = &stored_utxo {
                        assert_eq!(&stored_utxo.output, output);
                        assert_eq!(stored_utxo.height, query_height);

                        assert_eq!(
                            stored_utxo.from_coinbase,
                            transaction_location.index == TransactionIndex::from_usize(0),
                            "coinbase transactions must be the first transaction in a block:\n\
                             from_coinbase was: {from_coinbase},\n\
                             but transaction index was: {tx_index},\n\
                             at: {transaction_location:?},\n\
                             {output_location:?}",
                            from_coinbase = stored_utxo.from_coinbase,
                        );
                    }

                    // TODO: use output_location in #3151
                    stored_utxos.push((outpoint, stored_utxo));
                }
            }
        }

        // By definition, all of these lists should be in chain order.
        assert!(
            is_sorted(&stored_block_hashes),
            "unsorted: {:?}",
            stored_block_hashes
        );
        assert!(
            is_sorted(&stored_transactions),
            "unsorted: {:?}",
            stored_transactions
        );

        // The blocks, trees, transactions, and their hashes are in height/index order,
        // and we want to snapshot that order.
        // So we don't sort the vectors before snapshotting.
        insta::assert_ron_snapshot!("block_hashes", stored_block_hashes);
        insta::assert_ron_snapshot!("blocks", stored_blocks);

        insta::assert_ron_snapshot!("transaction_hashes", stored_transaction_hashes);
        insta::assert_ron_snapshot!("transactions", stored_transactions);

        insta::assert_ron_snapshot!("utxos", stored_utxos);

        // These snapshots will change if the trees do not have cached roots.
        // But we expect them to always have cached roots,
        // because those roots are used to populate the anchor column families.
        insta::assert_ron_snapshot!("sapling_trees", stored_sapling_trees);
        insta::assert_ron_snapshot!("orchard_trees", stored_orchard_trees);

        // The zcash_history types used in this tree don't support serde.
        insta::assert_debug_snapshot!("history_tree", (max_height, history_tree_at_tip));
    }
}

/// Return true if `list` is sorted in ascending order.
///
/// TODO: replace with Vec::is_sorted when it stabilises
///       https://github.com/rust-lang/rust/issues/53485
pub fn is_sorted<T: Ord + Clone>(list: &[T]) -> bool {
    // This could perform badly, but it is only used in tests, and the test vectors are small.
    let mut sorted_list = list.to_owned();
    sorted_list.sort();

    list == sorted_list
}
