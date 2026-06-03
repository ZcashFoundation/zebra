//! Tests for the offline finalized-state rollback utility.
//!
//! The core test is a round-trip equivalence check: sync a finalized state to height `N`, roll it
//! back to `M`, and assert it is *semantically* equivalent to a state freshly synced to `M`. We
//! compare via queries (tip, value pool, note-commitment/history roots, transparent balances and
//! UTXOs, nullifier presence) rather than raw bytes. Reopening each database also runs the
//! de-duplicate-tree format check, which guards against rollback leaving a duplicate sapling or
//! orchard tree entry (it would brick the node on the next startup).

use std::{env, path::Path, sync::Arc};

use tempfile::TempDir;

use zebra_chain::{
    amount::{Amount, NonNegative, MAX_MONEY},
    block::{self, Block, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters as TestnetParameters},
        Network, NetworkKind, NetworkUpgrade,
    },
    serialization::{BytesInDisplayOrder, ZcashDeserializeInto},
    subtree::NoteCommitmentSubtree,
    transaction::{LockTime, Transaction},
    transparent::{self, Address, Input, OutPoint, Output, Script},
    LedgerState,
};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    preview_rollback_finalized_state, rollback_finalized_state,
    service::{
        arbitrary::PreparedChain,
        finalized_state::{CheckpointVerifiedBlock, FinalizedState, STATE_COLUMN_FAMILIES_IN_CODE},
    },
    DiskWriteBatch, RollbackFinalizedStateError, RollbackFinalizedStateOptions,
    SemanticallyVerifiedBlock, ZebraDb,
};

/// Number of proptest cases. Each case syncs and rolls back several on-disk databases, so the
/// default is low; raise it with `PROPTEST_CASES` for deeper coverage.
const DEFAULT_ROLLBACK_PROPTEST_CASES: u32 = 1;

/// Cap on the synced height, to keep the genesis-to-target treestate rebuild fast.
const MAX_SYNCED_BLOCKS: usize = 12;

fn proptest_cases() -> u32 {
    env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ROLLBACK_PROPTEST_CASES)
}

/// A persistent (non-ephemeral) state config rooted at `dir`, so the rollback utility can reopen
/// the same database by path after the syncing handle is dropped.
fn config_at(dir: &Path) -> Config {
    Config {
        cache_dir: dir.to_path_buf(),
        ephemeral: false,
        ..Config::default()
    }
}

fn height(n: usize) -> Height {
    Height(u32::try_from(n).expect("test heights fit in u32"))
}

/// Syncs a fresh finalized state at `config` by committing `blocks` in order, then drops it so the
/// database lock is released for the rollback utility to reopen.
fn sync_to(config: &Config, network: &Network, blocks: &[SemanticallyVerifiedBlock]) {
    let mut state = FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    for block in blocks {
        let checkpoint_verified = CheckpointVerifiedBlock::from(block.block.clone());
        state
            .commit_finalized_direct(checkpoint_verified.into(), None, "rollback test")
            .expect("committing a generated block to a fresh state succeeds");
    }
}

/// Reopens the finalized state at `config` for read queries.
fn reopen(config: &Config, network: &Network) -> FinalizedState {
    FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
}

/// Opens the database at `config` directly, skipping format upgrades and their validation. The
/// subtree test writes synthetic subtree entries that don't correspond to real notes, which the
/// add-subtrees format check would reject.
fn open_unchecked_db(config: &Config, network: &Network) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
}

/// Asserts that `rolled` (a state rolled back to the tip of `retained`) is semantically equivalent
/// to `fresh` (a state synced only up to that tip), and that all data from the `removed` blocks is
/// gone from `rolled`.
fn assert_equivalent(
    rolled: &FinalizedState,
    fresh: &FinalizedState,
    network: &Network,
    retained: &[SemanticallyVerifiedBlock],
    removed: &[SemanticallyVerifiedBlock],
) {
    let (rolled, fresh) = (&rolled.db, &fresh.db);

    // Aggregates that fold in every committed block and note.
    assert_eq!(rolled.tip(), fresh.tip(), "tip");
    assert_eq!(
        rolled.finalized_value_pool(),
        fresh.finalized_value_pool(),
        "value pool"
    );
    assert_eq!(
        rolled.sprout_tree_for_tip().root(),
        fresh.sprout_tree_for_tip().root(),
        "sprout root"
    );
    assert_eq!(
        rolled.sapling_tree_for_tip().root(),
        fresh.sapling_tree_for_tip().root(),
        "sapling root"
    );
    assert_eq!(
        rolled.orchard_tree_for_tip().root(),
        fresh.orchard_tree_for_tip().root(),
        "orchard root"
    );
    assert_eq!(
        rolled.history_tree().hash(),
        fresh.history_tree().hash(),
        "history root"
    );

    // Transparent indexes and shielded nullifiers in the retained range must match a fresh sync.
    for block in retained {
        for transaction in &block.block.transactions {
            let tx_hash = transaction.hash();

            for (index, output) in transaction.outputs().iter().enumerate() {
                let outpoint = transparent::OutPoint::from_usize(tx_hash, index);
                assert_eq!(
                    rolled.utxo(&outpoint).is_some(),
                    fresh.utxo(&outpoint).is_some(),
                    "utxo presence for {outpoint:?}"
                );

                if let Some(address) = output.address(network) {
                    assert_eq!(
                        rolled.address_balance(&address),
                        fresh.address_balance(&address),
                        "balance for {address:?}"
                    );
                }
            }

            for nullifier in transaction.sprout_nullifiers() {
                assert!(
                    rolled.contains_sprout_nullifier(nullifier),
                    "retained sprout nullifier"
                );
            }
            for nullifier in transaction.sapling_nullifiers() {
                assert!(
                    rolled.contains_sapling_nullifier(nullifier),
                    "retained sapling nullifier"
                );
            }
            for nullifier in transaction.orchard_nullifiers() {
                assert!(
                    rolled.contains_orchard_nullifier(nullifier),
                    "retained orchard nullifier"
                );
            }
        }
    }

    // Everything from the removed blocks must be gone.
    for block in removed {
        assert!(
            rolled.height(block.hash).is_none(),
            "removed block {:?} is still present",
            block.hash
        );

        for transaction in &block.block.transactions {
            for nullifier in transaction.sprout_nullifiers() {
                assert!(
                    !rolled.contains_sprout_nullifier(nullifier),
                    "removed sprout nullifier"
                );
            }
            for nullifier in transaction.sapling_nullifiers() {
                assert!(
                    !rolled.contains_sapling_nullifier(nullifier),
                    "removed sapling nullifier"
                );
            }
            for nullifier in transaction.orchard_nullifiers() {
                assert!(
                    !rolled.contains_orchard_nullifier(nullifier),
                    "removed orchard nullifier"
                );
            }
        }
    }
}

/// Counts non-coinbase transparent inputs and shielded nullifiers across `blocks`, used to confirm
/// that a rolled-back range actually exercises the spend-reversal and nullifier-deletion paths
/// rather than passing over coinbase-only blocks.
fn count_reversible_features(blocks: &[SemanticallyVerifiedBlock]) -> (usize, usize) {
    let mut transparent_spends = 0;
    let mut shielded_nullifiers = 0;

    for block in blocks {
        for transaction in &block.block.transactions {
            transparent_spends += transaction
                .inputs()
                .iter()
                .filter(|input| input.outpoint().is_some())
                .count();
            shielded_nullifiers += transaction.sprout_nullifiers().count()
                + transaction.sapling_nullifiers().count()
                + transaction.orchard_nullifiers().count();
        }
    }

    (transparent_spends, shielded_nullifiers)
}

#[test]
fn rollback_matches_fresh_sync() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        // no_shrink: a failure points at the rollback logic, not the chain shape, and re-syncing
        // databases to shrink would be slow with little diagnostic value.
        |((chain, _count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            // Use the whole generated chain. Transparent outputs only become spendable once the
            // coinbase maturity (100 blocks) has elapsed, so spends appear only near the tip; a
            // short prefix would never exercise the transparent spend-reversal path.
            let synced: Vec<SemanticallyVerifiedBlock> = chain.iter().cloned().collect();
            prop_assume!(synced.len() > 2);
            let tip = synced.len() - 1;

            // Roll back to genesis (reverses every block) and to the middle. Both ranges include
            // the spend-bearing tail near the tip.
            for target in [0, tip / 2] {
                let removed = &synced[target + 1..];

                // Guard against a vacuous pass: both ranges must actually reverse transparent
                // spends and delete shielded nullifiers, not just drop coinbase-only blocks.
                let (transparent_spends, shielded_nullifiers) = count_reversible_features(removed);
                prop_assert!(
                    transparent_spends > 0,
                    "rolled-back range must exercise transparent spend reversal"
                );
                prop_assert!(
                    shielded_nullifiers > 0,
                    "rolled-back range must exercise shielded nullifier deletion"
                );

                let synced_dir = TempDir::new().expect("temp dir");
                let fresh_dir = TempDir::new().expect("temp dir");
                let synced_config = config_at(synced_dir.path());
                let fresh_config = config_at(fresh_dir.path());

                sync_to(&synced_config, &network, &synced);
                sync_to(&fresh_config, &network, &synced[..=target]);

                let summary = rollback_finalized_state(
                    synced_config.clone(),
                    &network,
                    RollbackFinalizedStateOptions {
                        target_height: height(target),
                        keep_rolled_back_blocks: false,
                        max_checkpoint_height: None,
                    },
                )
                .expect("rollback to a valid lower height succeeds");

                prop_assert_eq!(summary.new_tip.0, height(target));
                prop_assert_eq!(summary.rolled_back_count, height(tip).0 - height(target).0);

                assert_equivalent(
                    &reopen(&synced_config, &network),
                    &reopen(&fresh_config, &network),
                    &network,
                    &synced[..=target],
                    removed,
                );
            }
        }
    );

    Ok(())
}

/// Returns the lowest height below the tip whose sapling and orchard trees match the previous
/// block — i.e. the block added no shielded notes, so the forward write stored no new tree there.
/// Rolling back to such a height is the case that would leave a duplicate de-duplicated tree entry
/// if the rollback re-wrote the tip trees.
fn first_unchanged_tree_height(state: &FinalizedState, tip: usize) -> Option<Height> {
    (1..tip).find_map(|n| {
        let (h, prev) = (height(n), height(n - 1));
        let sapling_unchanged = state.db.sapling_tree_by_height(&h).map(|tree| tree.root())
            == state
                .db
                .sapling_tree_by_height(&prev)
                .map(|tree| tree.root());
        let orchard_unchanged = state.db.orchard_tree_by_height(&h).map(|tree| tree.root())
            == state
                .db
                .orchard_tree_by_height(&prev)
                .map(|tree| tree.root());
        (sapling_unchanged && orchard_unchanged).then_some(h)
    })
}

#[test]
fn rollback_avoids_duplicate_trees() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, _count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            let synced: Vec<SemanticallyVerifiedBlock> = chain.iter().cloned().collect();
            prop_assume!(synced.len() > 2);
            let tip = synced.len() - 1;

            let dir = TempDir::new().expect("temp dir");
            let config = config_at(dir.path());
            sync_to(&config, &network, &synced);

            // Roll back to a height whose block added no shielded notes. The de-duplicated tree
            // column families store no entry there, so re-writing the tip tree at that height
            // would duplicate the most recent stored tree.
            let target = {
                let synced_state = reopen(&config, &network);
                first_unchanged_tree_height(&synced_state, tip)
            };
            prop_assume!(target.is_some());
            let target = target.expect("checked is_some above");

            rollback_finalized_state(
                config.clone(),
                &network,
                RollbackFinalizedStateOptions {
                    target_height: target,
                    keep_rolled_back_blocks: false,
                    max_checkpoint_height: None,
                },
            )
            .expect("rollback to an unchanged-tree height succeeds");

            // Reopening runs the de-duplicate-tree format check, which panics on a duplicate entry.
            let rolled = reopen(&config, &network);
            prop_assert_eq!(rolled.db.tip().map(|(tip_height, _)| tip_height), Some(target));
        }
    );

    Ok(())
}

#[test]
fn rollback_rejects_invalid_requests() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            let synced: Vec<SemanticallyVerifiedBlock> =
                chain.iter().take(count.min(MAX_SYNCED_BLOCKS)).cloned().collect();
            prop_assume!(synced.len() >= 2);
            let tip = synced.len() - 1;

            let synced_dir = TempDir::new().expect("temp dir");
            let synced_config = config_at(synced_dir.path());
            sync_to(&synced_config, &network, &synced);

            // The validation errors surface through the read-only preview path, so none of these
            // mutate the database.
            let preview = |target: usize, keep: bool, max: Option<Height>| {
                preview_rollback_finalized_state(
                    synced_config.clone(),
                    &network,
                    RollbackFinalizedStateOptions {
                        target_height: height(target),
                        keep_rolled_back_blocks: keep,
                        max_checkpoint_height: max,
                    },
                )
            };

            prop_assert!(
                matches!(
                    preview(tip, false, None),
                    Err(RollbackFinalizedStateError::TargetIsTip { .. })
                ),
                "rolling back to the tip is rejected"
            );
            prop_assert!(
                matches!(
                    preview(tip + 1, false, None),
                    Err(RollbackFinalizedStateError::TargetAboveTip { .. })
                ),
                "rolling back above the tip is rejected"
            );
            // Bug #2: keeping blocks below the max checkpoint height would silently drop them.
            prop_assert!(
                matches!(
                    preview(tip - 1, true, Some(height(tip))),
                    Err(RollbackFinalizedStateError::KeepBelowMaxCheckpoint { .. })
                ),
                "keeping blocks below the max checkpoint is rejected"
            );
            // The same target is allowed once it is at/above the max checkpoint height.
            prop_assert!(
                preview(tip - 1, true, Some(height(tip - 1))).is_ok(),
                "keeping blocks at the max checkpoint is allowed"
            );

            // An empty database has no tip to roll back from.
            let empty_dir = TempDir::new().expect("temp dir");
            let empty_config = config_at(empty_dir.path());
            sync_to(&empty_config, &network, &[]);
            prop_assert!(
                matches!(
                    preview_rollback_finalized_state(
                        empty_config,
                        &network,
                        RollbackFinalizedStateOptions {
                            target_height: Height(0),
                            keep_rolled_back_blocks: false,
                            max_checkpoint_height: None,
                        },
                    ),
                    Err(RollbackFinalizedStateError::EmptyState)
                ),
                "rolling back an empty state is rejected"
            );
        }
    );

    Ok(())
}

#[test]
fn rollback_keeps_blocks_for_restore() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            let synced: Vec<SemanticallyVerifiedBlock> =
                chain.iter().take(count.min(MAX_SYNCED_BLOCKS)).cloned().collect();
            prop_assume!(synced.len() >= 3);
            let tip = synced.len() - 1;
            let target = tip - 2;

            let synced_dir = TempDir::new().expect("temp dir");
            let synced_config = config_at(synced_dir.path());
            sync_to(&synced_config, &network, &synced);

            let summary = rollback_finalized_state(
                synced_config,
                &network,
                RollbackFinalizedStateOptions {
                    target_height: height(target),
                    keep_rolled_back_blocks: true,
                    // A zero max checkpoint height keeps the request above the checkpoint guard.
                    max_checkpoint_height: Some(Height(0)),
                },
            )
            .expect("keeping rolled-back blocks succeeds above the max checkpoint");

            let backup = summary
                .backup
                .expect("keep_rolled_back_blocks records a backup summary");
            prop_assert_eq!(backup.block_count, tip - target);
            prop_assert!(backup.path.exists(), "backup directory was written");
        }
    );

    Ok(())
}

/// A V1 coinbase transaction at `height` paying `value` to `address`. The miner data pads the
/// coinbase script past `MIN_COINBASE_SCRIPT_LEN` so it round-trips through the database.
fn coinbase_tx(height: Height, value: Amount<NonNegative>, address: &Address) -> Arc<Transaction> {
    Arc::new(Transaction::V1 {
        inputs: vec![Input::Coinbase {
            height,
            data: vec![0x00; 8],
            sequence: u32::MAX,
        }],
        outputs: vec![Output::new(value, address.script())],
        lock_time: LockTime::unlocked(),
    })
}

/// A V1 transaction spending `outpoint` and paying `value` to `address`.
fn spend_tx(outpoint: OutPoint, value: Amount<NonNegative>, address: &Address) -> Arc<Transaction> {
    Arc::new(Transaction::V1 {
        inputs: vec![Input::PrevOut {
            outpoint,
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        }],
        outputs: vec![Output::new(value, address.script())],
        lock_time: LockTime::unlocked(),
    })
}

/// Builds a child of `parent` containing `transactions` (whose coinbase encodes the new height).
/// The checkpoint commit doesn't validate the pre-Sapling commitment or merkle root, so the
/// parent's header is reused with only the parent hash updated.
fn child_block(parent: &Block, transactions: Vec<Arc<Transaction>>) -> Arc<Block> {
    let header = block::Header {
        previous_block_hash: parent.hash(),
        ..*parent.header
    };
    Arc::new(Block {
        header: Arc::new(header),
        transactions,
    })
}

/// A V4 transaction with one dummy Sprout JoinSplit whose two commitments grow the Sprout note
/// commitment tree. The checkpoint commit path doesn't validate the JoinSplit, so the rest is
/// zeroed.
fn sprout_joinsplit_tx() -> Arc<Transaction> {
    let joinsplit = zebra_chain::sprout::JoinSplit::<zebra_chain::primitives::Groth16Proof> {
        vpub_old: Amount::zero(),
        vpub_new: Amount::zero(),
        anchor: zebra_chain::sprout::tree::Root::default(),
        nullifiers: [
            zebra_chain::sprout::note::Nullifier::from([0; 32]),
            zebra_chain::sprout::note::Nullifier::from([1; 32]),
        ],
        commitments: [
            zebra_chain::sprout::commitment::NoteCommitment::from([2; 32]),
            zebra_chain::sprout::commitment::NoteCommitment::from([3; 32]),
        ],
        ephemeral_key: zebra_chain::primitives::x25519::PublicKey::from([0; 32]),
        random_seed: zebra_chain::sprout::RandomSeed::from([0; 32]),
        vmacs: [
            zebra_chain::sprout::note::Mac::from([0; 32]),
            zebra_chain::sprout::note::Mac::from([0; 32]),
        ],
        zkproof: zebra_chain::primitives::Groth16Proof::from([0; 192]),
        enc_ciphertexts: [
            zebra_chain::sprout::note::EncryptedNote([0; 601]),
            zebra_chain::sprout::note::EncryptedNote([0; 601]),
        ],
    };

    Arc::new(Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::unlocked(),
        expiry_height: Height(0),
        joinsplit_data: Some(zebra_chain::transaction::JoinSplitData {
            first: joinsplit,
            rest: Vec::new(),
            pub_key: zebra_chain::primitives::ed25519::VerificationKeyBytes::from([0; 32]),
            sig: zebra_chain::primitives::ed25519::Signature::from([0; 64]),
        }),
        sapling_shielded_data: None,
    })
}

fn child_block_with_history_commitment(
    parent: &Block,
    transactions: Vec<Arc<Transaction>>,
    network: &Network,
    previous_history_tree: &zebra_chain::history_tree::HistoryTree,
) -> Arc<Block> {
    let mut block = child_block(parent, transactions);
    let height = block
        .coinbase_height()
        .expect("test block must have a coinbase height");
    let history_tree_root = previous_history_tree
        .hash()
        .expect("modern test block must have a previous history tree root");

    let commitment_bytes = if NetworkUpgrade::current(network, height) >= NetworkUpgrade::Nu5 {
        zebra_chain::block::ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            &history_tree_root,
            &block.auth_data_root(),
        )
        .bytes_in_serialized_order()
    } else {
        history_tree_root.bytes_in_serialized_order()
    };

    let block_mut = Arc::make_mut(&mut block);
    Arc::make_mut(&mut block_mut.header).commitment_bytes = commitment_bytes.into();

    block
}

/// Rolling back a same-address transparent self-spend chain must not transiently push the address
/// balance above `MAX_MONEY`. The buggy reversal re-added every spent input before subtracting the
/// created outputs, so the running balance reached `1.5 * MAX_MONEY` and aborted; the fix undoes
/// each transaction in block order (un-credit then un-debit), keeping it in `[0, MAX_MONEY]`.
#[test]
fn rollback_reverses_intra_block_self_spend() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]);
    let throwaway = Address::from_script_hash(NetworkKind::Mainnet, [0x17; 20]);
    let value = Amount::<NonNegative>::try_from(MAX_MONEY / 2)
        .expect("MAX_MONEY / 2 fits in Amount<NonNegative>");
    let dust = Amount::<NonNegative>::try_from(1).expect("1 fits in Amount<NonNegative>");

    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("mainnet genesis test vector deserializes");

    // Height 1: a coinbase paying `value` to the address.
    let block1 = child_block(&genesis, vec![coinbase_tx(Height(1), value, &address)]);
    let block1_coinbase = OutPoint {
        hash: block1.transactions[0].hash(),
        index: 0,
    };

    // Height 2: spend the height-1 coinbase back to the same address, then spend that output to the
    // same address again. With the coinbase this is a same-address self-spend chain.
    let t0 = spend_tx(block1_coinbase, value, &address);
    let t0_output = OutPoint {
        hash: t0.hash(),
        index: 0,
    };
    let t1 = spend_tx(t0_output, value, &address);
    let block2 = child_block(
        &block1,
        vec![coinbase_tx(Height(2), dust, &throwaway), t0, t1],
    );

    let chain: Vec<SemanticallyVerifiedBlock> = [genesis, block1, block2]
        .into_iter()
        .map(SemanticallyVerifiedBlock::from)
        .collect();

    let dir = TempDir::new().expect("temp dir");
    let config = config_at(dir.path());
    sync_to(&config, &network, &chain);

    // Roll back to height 1. Reversing height 2 re-credits the address's two spent inputs
    // (2 * value) on top of its current balance (value); the buggy order reached 3 * value.
    rollback_finalized_state(
        config.clone(),
        &network,
        RollbackFinalizedStateOptions {
            target_height: Height(1),
            keep_rolled_back_blocks: false,
            max_checkpoint_height: None,
        },
    )
    .expect("reversing a same-address self-spend chain must not overflow the address balance");

    let rolled = reopen(&config, &network);
    assert_eq!(rolled.db.tip().map(|(h, _)| h), Some(Height(1)), "tip");
    assert_eq!(
        rolled
            .db
            .address_balance(&address)
            .map(|(balance, _)| balance),
        Some(value),
        "address balance is restored to its height-1 value",
    );
}

/// When a rolled-back block grew the Sprout tree, rollback must reset the Sprout tip to the target
/// height's tree, not leave it at the later tip.
#[test]
fn rollback_resets_sprout_tree_changed_in_range() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]);
    let dust = Amount::<NonNegative>::try_from(1).expect("1 fits in Amount<NonNegative>");

    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("mainnet genesis test vector deserializes");
    let block1 = child_block(&genesis, vec![coinbase_tx(Height(1), dust, &address)]);
    let block2 = child_block(
        &block1,
        vec![
            coinbase_tx(Height(2), dust, &address),
            sprout_joinsplit_tx(),
        ],
    );

    let to_height_1: Vec<SemanticallyVerifiedBlock> = [genesis.clone(), block1.clone()]
        .into_iter()
        .map(SemanticallyVerifiedBlock::from)
        .collect();
    let to_height_2: Vec<SemanticallyVerifiedBlock> = [genesis, block1, block2]
        .into_iter()
        .map(SemanticallyVerifiedBlock::from)
        .collect();

    let sprout_root_at = |blocks: &[SemanticallyVerifiedBlock]| {
        let dir = TempDir::new().expect("temp dir");
        let config = config_at(dir.path());
        sync_to(&config, &network, blocks);
        open_unchecked_db(&config, &network)
            .sprout_tree_for_tip()
            .root()
    };
    let root_at_1 = sprout_root_at(&to_height_1);
    let root_at_2 = sprout_root_at(&to_height_2);
    assert_ne!(
        root_at_1, root_at_2,
        "the JoinSplit must grow the sprout tree in the rolled-back range"
    );

    let dir = TempDir::new().expect("temp dir");
    let config = config_at(dir.path());
    sync_to(&config, &network, &to_height_2);
    rollback_finalized_state(
        config.clone(),
        &network,
        RollbackFinalizedStateOptions {
            target_height: Height(1),
            keep_rolled_back_blocks: false,
            max_checkpoint_height: None,
        },
    )
    .expect("rollback succeeds");

    assert_eq!(
        open_unchecked_db(&config, &network)
            .sprout_tree_for_tip()
            .root(),
        root_at_1,
        "sprout tip reset to the target height, not left at the rolled-back tip",
    );
}

/// A configured testnet with early modern activation heights, so rollback tests can exercise
/// Canopy-and-later history roots without syncing hundreds of thousands of blocks.
fn modern_rollback_network() -> Network {
    TestnetParameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            before_overwinter: Some(1),
            overwinter: Some(2),
            sapling: Some(3),
            blossom: Some(4),
            heartwood: Some(5),
            canopy: Some(6),
            nu5: Some(7),
            nu6: Some(8),
            nu6_1: Some(9),
            nu6_2: Some(10),
            nu7: Some(11),
        })
        .expect("configured activation heights are valid")
        .extend_funding_streams()
        .to_network()
        .expect("configured network is valid")
}

/// Modern rollback targets must not replay note commitment trees from genesis. This test deletes
/// the genesis block header after syncing; a genesis-to-target treestate rebuild would fail to load
/// height 0, while the modern path only rebuilds the history tree from Canopy activation.
#[test]
fn modern_rollback_does_not_rebuild_note_trees_from_genesis() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = modern_rollback_network();
    let target_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 activation height is configured");
    let target_index = usize::try_from(target_height.0).expect("test height fits in usize");
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, Some(5), true);

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, _count, network, _history_tree) in PreparedChain::default()
            .with_ledger_strategy(ledger_strategy)
            .with_valid_commitments()
            .no_shrink())
            | {
            let synced: Vec<SemanticallyVerifiedBlock> = chain.iter().cloned().collect();
            prop_assume!(synced.len() > target_index + 1);
            prop_assert!(
                !synced[target_index + 1..]
                    .iter()
                    .any(|block| block.block.sprout_note_commitments().next().is_some()),
                "V5 blocks in the removed range must not contain Sprout commitments"
            );

            let synced_dir = TempDir::new().expect("temp dir");
            let fresh_dir = TempDir::new().expect("temp dir");
            let synced_config = config_at(synced_dir.path());
            let fresh_config = config_at(fresh_dir.path());

            sync_to(&synced_config, &network, &synced);
            sync_to(&fresh_config, &network, &synced[..=target_index]);

            let old_sprout_root = open_unchecked_db(&synced_config, &network)
                .sprout_tree_for_tip()
                .root();

            {
                let db = open_unchecked_db(&synced_config, &network);
                let mut batch = DiskWriteBatch::new();
                batch.delete_block_header(&db, Height(0));
                db.write_batch(batch)
                    .expect("database accepts the synthetic missing genesis header");
            }

            rollback_finalized_state(
                synced_config.clone(),
                &network,
                RollbackFinalizedStateOptions {
                    target_height,
                    keep_rolled_back_blocks: false,
                    max_checkpoint_height: None,
                },
            )
            .expect("modern rollback does not need the genesis block");

            let rolled = open_unchecked_db(&synced_config, &network);
            let fresh = reopen(&fresh_config, &network);

            prop_assert_eq!(rolled.tip(), fresh.db.tip(), "tip");
            prop_assert_eq!(
                rolled.finalized_value_pool(),
                fresh.db.finalized_value_pool(),
                "value pool"
            );
            prop_assert_eq!(
                rolled.sprout_tree_for_tip().root(),
                old_sprout_root,
                "modern rollback keeps the current Sprout tip tree"
            );
            prop_assert_eq!(
                rolled.sapling_tree_for_tip().root(),
                fresh.db.sapling_tree_for_tip().root(),
                "sapling root"
            );
            prop_assert_eq!(
                rolled.orchard_tree_for_tip().root(),
                fresh.db.orchard_tree_for_tip().root(),
                "orchard root"
            );
            prop_assert_eq!(
                rolled.history_tree().hash(),
                fresh.db.history_tree().hash(),
                "history root"
            );
        }
    );

    Ok(())
}

/// The same Sprout fallback must work for a modern target. If the removed range contains a Sprout
/// JoinSplit, rollback cannot use the Sprout tip tree as the target tree.
#[test]
fn modern_rollback_resets_sprout_tree_changed_in_range() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = modern_rollback_network();
    let target_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 activation height is configured");
    let target_index = usize::try_from(target_height.0).expect("test height fits in usize");
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, Some(5), true);

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, _count, network, _history_tree) in PreparedChain::default()
            .with_ledger_strategy(ledger_strategy)
            .with_valid_commitments()
            .no_shrink())
            | {
            let prefix: Vec<SemanticallyVerifiedBlock> =
                chain.iter().take(target_index + 1).cloned().collect();
            prop_assume!(prefix.len() == target_index + 1);

            let target_dir = TempDir::new().expect("temp dir");
            let target_config = config_at(target_dir.path());
            sync_to(&target_config, &network, &prefix);

            let target_db = open_unchecked_db(&target_config, &network);
            let parent = prefix
                .last()
                .expect("target prefix has a tip block")
                .block
                .clone();
            let previous_history_tree = target_db.history_tree();
            let root_at_target = target_db.sprout_tree_for_tip().root();
            drop(target_db);

            let removed_height = (target_height + 1).expect("test target height is not max");
            let address = Address::from_script_hash(NetworkKind::Testnet, [0x42; 20]);
            let dust = Amount::<NonNegative>::try_from(1).expect("1 fits in Amount<NonNegative>");
            let sprout_block = child_block_with_history_commitment(
                &parent,
                vec![
                    coinbase_tx(removed_height, dust, &address),
                    sprout_joinsplit_tx(),
                ],
                &network,
                &previous_history_tree,
            );

            let mut synced = prefix;
            synced.push(SemanticallyVerifiedBlock::from(sprout_block));

            let sprout_dir = TempDir::new().expect("temp dir");
            let sprout_config = config_at(sprout_dir.path());
            sync_to(&sprout_config, &network, &synced);
            let root_at_tip = open_unchecked_db(&sprout_config, &network)
                .sprout_tree_for_tip()
                .root();
            prop_assert_ne!(
                root_at_target,
                root_at_tip,
                "the synthetic JoinSplit must grow Sprout after the modern target"
            );

            rollback_finalized_state(
                sprout_config.clone(),
                &network,
                RollbackFinalizedStateOptions {
                    target_height,
                    keep_rolled_back_blocks: false,
                    max_checkpoint_height: None,
                },
            )
            .expect("modern rollback with Sprout in the removed range succeeds");

            prop_assert_eq!(
                open_unchecked_db(&sprout_config, &network)
                    .sprout_tree_for_tip()
                    .root(),
                root_at_target,
                "modern rollback reset Sprout to the target height"
            );
        }
    );

    Ok(())
}

/// Rollback must prune note-commitment subtrees whose `end_height` is above the new tip, and keep
/// those at or below it. A subtree completes only every 2^16 notes, far more than any test chain,
/// so the subtree entries are inserted directly into the database.
#[test]
fn rollback_prunes_subtrees_above_target() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]);
    let dust = Amount::<NonNegative>::try_from(1).expect("1 fits in Amount<NonNegative>");

    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("mainnet genesis test vector deserializes");
    let block1 = child_block(&genesis, vec![coinbase_tx(Height(1), dust, &address)]);
    let block2 = child_block(&block1, vec![coinbase_tx(Height(2), dust, &address)]);
    let block3 = child_block(&block2, vec![coinbase_tx(Height(3), dust, &address)]);

    let chain: Vec<SemanticallyVerifiedBlock> = [genesis, block1, block2, block3]
        .into_iter()
        .map(SemanticallyVerifiedBlock::from)
        .collect();

    let dir = TempDir::new().expect("temp dir");
    let config = config_at(dir.path());
    sync_to(&config, &network, &chain);

    // Insert subtrees straddling the rollback target (height 2): index 0 ends at/below it (kept),
    // index 1 ends above it (pruned).
    let sapling_node = sapling_crypto::Node::from_bytes([0; 32]).expect("dummy sapling node");
    let orchard_node = zebra_chain::orchard::tree::Node::default();
    {
        let db = open_unchecked_db(&config, &network);
        let mut batch = DiskWriteBatch::new();
        batch.insert_sapling_subtree(
            &db,
            &NoteCommitmentSubtree::new(0u16, Height(2), sapling_node),
        );
        batch.insert_sapling_subtree(
            &db,
            &NoteCommitmentSubtree::new(1u16, Height(3), sapling_node),
        );
        batch.insert_orchard_subtree(
            &db,
            &NoteCommitmentSubtree::new(0u16, Height(1), orchard_node),
        );
        batch.insert_orchard_subtree(
            &db,
            &NoteCommitmentSubtree::new(1u16, Height(3), orchard_node),
        );
        db.write_batch(batch)
            .expect("database accepts the synthetic subtree batch");
    }

    rollback_finalized_state(
        config.clone(),
        &network,
        RollbackFinalizedStateOptions {
            target_height: Height(2),
            keep_rolled_back_blocks: false,
            max_checkpoint_height: None,
        },
    )
    .expect("rollback succeeds");

    let rolled = open_unchecked_db(&config, &network);
    let sapling: Vec<u16> = rolled
        .sapling_subtree_list_by_index_range(..)
        .keys()
        .map(|index| index.0)
        .collect();
    let orchard: Vec<u16> = rolled
        .orchard_subtree_list_by_index_range(..)
        .keys()
        .map(|index| index.0)
        .collect();

    assert_eq!(
        sapling,
        vec![0],
        "sapling subtree above the target is pruned"
    );
    assert_eq!(
        orchard,
        vec![0],
        "orchard subtree above the target is pruned"
    );
}
