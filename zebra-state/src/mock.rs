//! Development-only fixtures for a local Regtest node.
//!
//! Blocks are committed directly to an empty database without consensus verification.
//! The generated spends and shielded rows are synthetic, not a valid chain history.

use std::{collections::VecDeque, fs, path::PathBuf};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Height},
    block_info::BlockInfo,
    ironwood, orchard,
    parameters::{Network, NetworkKind, NetworkUpgrade},
    sapling,
    serialization::ZcashDeserialize,
    sprout,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::{LockTime, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{FinalizedState, MAX_ON_DISK_HEIGHT},
    state_database_format_version_in_code, BoxError, Config, DiskWriteBatch, HashOrHeight,
    WriteDisk,
};

mod blocks;
use blocks::{child, commit};

#[cfg(feature = "snapshot-trial")]
mod serving;
#[cfg(feature = "snapshot-trial")]
pub use serving::{prepare_header_context, prepare_serving_state};

const COINBASE_MATURITY: u32 = 100;
const SPEND_INTERVAL: u32 = 7;
const COINBASE_VALUE: u64 = 1_000;
const SAPLING_SUBTREE_HEIGHT: u32 = 8;
const ORCHARD_SUBTREE_HEIGHT: u32 = 20;
const SHIELDED_TREE_HEIGHT: u32 = 30;
const NULLIFIERS_PER_POOL: u32 = 10_000;

/// Builds a populated Regtest database and returns the finalized tip hash.
///
/// The cache directory must not exist. Activate NU5 and NU6.3 from height 1
/// and open the result with the same Regtest parameters.
pub fn build(
    cache_dir: PathBuf,
    network: &Network,
    height: Height,
) -> Result<block::Hash, BoxError> {
    if !(Height(COINBASE_MATURITY + SPEND_INTERVAL)..=MAX_ON_DISK_HEIGHT).contains(&height) {
        return Err(format!(
            "mock height must be between {} and {}",
            COINBASE_MATURITY + SPEND_INTERVAL,
            MAX_ON_DISK_HEIGHT.0
        )
        .into());
    }
    if !network.is_regtest()
        || [NetworkUpgrade::Nu5, NetworkUpgrade::Nu6_3]
            .into_iter()
            .any(|upgrade| upgrade.activation_height(network) != Some(Height(1)))
    {
        return Err("mock state requires Regtest with NU5 and NU6.3 active at height 1".into());
    }

    fs::create_dir(&cache_dir)?;
    let config = Config {
        cache_dir,
        ..Config::default()
    };
    let mut state = FinalizedState::new_with_debug(
        &config,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    // The builder skips background format upgrades and writes the current version itself.
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())?;
    state.db.mark_finished_format_upgrades();

    let mut parent = block::genesis::regtest_genesis_block();
    commit(&mut state, parent.clone())?;
    let mut mature_candidates = VecDeque::new();

    for block_height in 1..=height.0 {
        let address = transparent::Address::from_pub_key_hash(
            NetworkKind::Regtest,
            [u8::try_from(block_height % 10 + 1)?; 20],
        );
        let block_height = Height(block_height);
        let coinbase = transparent::Output {
            value: zats(COINBASE_VALUE + u64::from(block_height.0))?,
            lock_script: address.script(),
        };
        let mut transactions = Vec::new();
        if block_height.0 % SPEND_INTERVAL == 0
            && mature_candidates
                .front()
                .is_some_and(|(created, _)| block_height.0 - created >= COINBASE_MATURITY)
        {
            let (created, outpoint) = mature_candidates
                .pop_front()
                .expect("the first coinbase is mature");
            transactions.push(Transaction::test_v5(
                NetworkUpgrade::current(network, block_height),
                vec![transparent::Input::PrevOut {
                    outpoint,
                    unlock_script: transparent::Script::new(&[]),
                    sequence: u32::MAX,
                }],
                vec![
                    transparent::Output {
                        value: zats(COINBASE_VALUE + u64::from(created) - 1)?,
                        lock_script: address.script(),
                    },
                    transparent::Output {
                        value: zats(1)?,
                        lock_script: transparent::Script::new(&[]),
                    },
                ],
                LockTime::unlocked(),
                block_height,
            ));
        }
        if block_height == height {
            // The test bundle has a dummy proof and exists only as local fixture data.
            let tx = Transaction::test_v5(
                NetworkUpgrade::current(network, block_height),
                Vec::new(),
                Vec::new(),
                LockTime::unlocked(),
                block_height,
            );
            transactions.push(
                zebra_chain::transaction::arbitrary::shielded::insert_fake_orchard_shielded_data(
                    tx,
                ),
            );
        }
        let block = child(network, &state, &parent, vec![coinbase], transactions);
        mature_candidates.push_back((
            block_height.0,
            transparent::OutPoint {
                hash: block.transactions[0].hash(),
                index: 0,
            },
        ));
        commit(&mut state, block.clone())?;
        populate_shielded_rows(&state, block_height)?;
        parent = block;
    }

    set_shielded_value_pools(&state, height)?;
    Ok(parent.hash())
}

fn zats(value: u64) -> Result<Amount<NonNegative>, BoxError> {
    Amount::try_from(value).map_err(Into::into)
}

fn populate_shielded_rows(state: &FinalizedState, height: Height) -> Result<(), BoxError> {
    let db = &state.db;
    let batch = match height.0 {
        SAPLING_SUBTREE_HEIGHT => {
            let tree = sapling_tree_at((1 << 16) - 1);
            let (index, root) = tree
                .completed_subtree_index_and_root()
                .expect("the mock Sapling frontier completes the first subtree");
            let mut batch = DiskWriteBatch::new();
            batch.create_sapling_tree(db, &height, &tree);
            let subtrees = db
                .cf_handle("sapling_note_commitment_subtree")
                .expect("the state database has a Sapling subtree column family");
            batch.zs_insert(
                &subtrees,
                index,
                NoteCommitmentSubtreeData::new(height, root),
            );
            batch
        }
        ORCHARD_SUBTREE_HEIGHT => {
            let tree = orchard_tree(&[9]);
            let root = orchard::tree::Node::try_from([9; 32])?;
            let mut batch = DiskWriteBatch::new();
            batch.create_orchard_tree(db, &height, &tree);
            batch.create_ironwood_tree(db, &height, &tree);
            for column in [
                "orchard_note_commitment_subtree",
                "ironwood_note_commitment_subtree",
            ] {
                let subtrees = db
                    .cf_handle(column)
                    .expect("the state database has this subtree column family");
                batch.zs_insert(
                    &subtrees,
                    NoteCommitmentSubtreeIndex(0),
                    NoteCommitmentSubtreeData::new(height, root),
                );
            }
            batch
        }
        SHIELDED_TREE_HEIGHT => {
            let mut sprout = sprout::tree::NoteCommitmentTree::default();
            for seed in [3, 4] {
                sprout.append(sprout::NoteCommitment::from([seed; 32]))?;
            }
            let mut batch = DiskWriteBatch::new();
            batch.update_sprout_tree(db, &sprout);
            batch.create_sapling_tree(db, &height, &sapling_tree_at((1 << 16) + 100));
            batch.create_orchard_tree(db, &height, &orchard_tree(&[1, 2]));
            batch.create_ironwood_tree(db, &height, &orchard_tree(&[5, 6]));
            insert_nullifiers(&mut batch, state);
            batch
        }
        _ => return Ok(()),
    };
    db.write_batch(batch)?;
    Ok(())
}

fn set_shielded_value_pools(state: &FinalizedState, height: Height) -> Result<(), BoxError> {
    let db = &state.db;
    let mut pools = db.finalized_value_pool();
    pools.set_sprout_value_balance(ValueBalance::from_sprout_amount(zats(200_000_000)?));
    pools.set_sapling_value_balance(ValueBalance::from_sapling_amount(zats(500_000_000)?));
    pools.set_orchard_value_balance(ValueBalance::from_orchard_amount(zats(300_000_000)?));
    pools.set_ironwood_value_balance(ValueBalance::from_ironwood_amount(zats(100_000_000)?));

    let size = db
        .block_info(HashOrHeight::Height(height))
        .expect("the builder just committed the tip block")
        .size();
    let mut batch = DiskWriteBatch::new();
    let value_pools = db
        .cf_handle("tip_chain_value_pool")
        .expect("the state database has a tip value pool column family");
    let block_infos = db
        .cf_handle("block_info")
        .expect("the state database has a block info column family");
    batch.zs_insert(&value_pools, (), pools);
    batch.zs_insert(&block_infos, height, BlockInfo::new(pools, size));
    db.write_batch(batch)?;
    Ok(())
}

fn insert_nullifiers(batch: &mut DiskWriteBatch, state: &FinalizedState) {
    let db = &state.db;
    let column = |name| {
        db.cf_handle(name)
            .expect("the state database has a nullifier column family for each pool")
    };
    let sprout = column("sprout_nullifiers");
    let sapling = column("sapling_nullifiers");
    let orchard = column("orchard_nullifiers");
    let ironwood = column("ironwood_nullifiers");

    for index in 0..NULLIFIERS_PER_POOL {
        let bytes = |pool| {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&index.to_le_bytes());
            bytes[4] = pool;
            bytes
        };
        batch.zs_insert(&sprout, sprout::Nullifier::from(bytes(1)), ());
        batch.zs_insert(&sapling, sapling::Nullifier::from(bytes(2)), ());
        let orchard_nullifier = orchard::Nullifier::try_from(bytes(3))
            .expect("the Orchard fixture bytes encode a canonical field element");
        batch.zs_insert(&orchard, orchard_nullifier, ());
        let ironwood_nullifier = orchard::Nullifier::try_from(bytes(4))
            .expect("the Ironwood fixture bytes encode a canonical field element");
        batch.zs_insert(&ironwood, ironwood::Nullifier::from(ironwood_nullifier), ());
    }
}

/// Constructs a small Sapling frontier at a chosen position without filling the tree.
fn sapling_tree_at(position: u64) -> sapling::tree::NoteCommitmentTree {
    use sapling::tree::legacy::{
        LegacyFrontier, LegacyLeaf, LegacyNonEmptyFrontier, LegacyNoteCommitmentTree,
        LegacyPosition, Node,
    };

    let node = |seed: u8| {
        let mut bytes = [0; 32];
        bytes[0] = seed;
        Node(
            Option::from(sapling_crypto::Node::from_bytes(bytes))
                .expect("small fixture values are valid Sapling nodes"),
        )
    };
    // A right leaf holds the first ommer; the remaining set bits count the rest.
    let ommers = position.count_ones() - u32::from(position % 2 == 1);
    let leaf = if position % 2 == 1 {
        LegacyLeaf::Right(node(3), node(4))
    } else {
        LegacyLeaf::Left(node(3))
    };
    let mut legacy = LegacyNoteCommitmentTree::from(sapling::tree::NoteCommitmentTree::default());
    legacy.inner = LegacyFrontier {
        frontier: Some(LegacyNonEmptyFrontier {
            position: LegacyPosition(
                usize::try_from(position).expect("the mock tree position fits in usize"),
            ),
            leaf,
            ommers: (0..ommers)
                .map(|ommer| node(10 + u8::try_from(ommer).expect("there are at most 64 bits")))
                .collect(),
        }),
    };
    legacy.into()
}

fn orchard_tree(seeds: &[u8]) -> orchard::tree::NoteCommitmentTree {
    let mut tree = orchard::tree::NoteCommitmentTree::default();
    for &seed in seeds {
        let mut bytes = [0; 32];
        bytes[0] = seed;
        let update = orchard::tree::NoteCommitmentUpdate::zcash_deserialize(&bytes[..])
            .expect("small fixture values are canonical Orchard commitments");
        tree.append(update)
            .expect("the mock Orchard tree starts empty");
    }
    tree
}
