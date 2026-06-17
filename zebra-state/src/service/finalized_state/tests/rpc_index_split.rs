//! Tests for the consensus / RPC-only finalized write split
//! ([`Config::separate_rpc_index_db`]).
//!
//! Covers:
//! - column-family placement: with the split on, the consensus database is
//!   opened without the RPC-only column families, and the RPC-only data is
//!   written to the separate RPC index database;
//! - RPC-read parity: once the trailing indexer catches up, the RPC-only read
//!   accessors (`address_balance`, `address_utxos`, `address_tx_ids`) on a
//!   split state return the same results as a single-database baseline;
//! - crash-safety catch-up: an RPC index database behind the consensus tip is
//!   replayed forward up to the consensus tip from the consensus database.
//!
//! See `docs/design/state-write-split.md`.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative},
    block::{self, Block, Height},
    parameters::{Network, NetworkKind},
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
    transparent::{
        self, new_ordered_outputs_with_height, Address, Input, OutPoint, Output, Script,
    },
};

use crate::{
    request::{FinalizedBlock, SemanticallyVerifiedBlock, Treestate},
    service::finalized_state::{
        disk_db::DiskWriteBatch,
        disk_format::transparent::{
            AddressBalanceLocation, AddressBalanceLocationUpdates, OutputLocation,
        },
        zebra_db::transparent::TransparentBatchKind,
        FinalizedState, ZebraDb, CONSENSUS_COLUMN_FAMILIES_IN_CODE,
        RPC_INDEX_COLUMN_FAMILIES_IN_CODE,
    },
    CheckpointVerifiedBlock, Config,
};

/// A config with the consensus / RPC write split enabled, on an ephemeral
/// database.
fn split_config() -> Config {
    Config {
        separate_rpc_index_db: true,
        ..Config::ephemeral()
    }
}

/// Commits the mainnet genesis and blocks `1..=max_height` to a fresh finalized
/// state with `config`, returning the state and the committed blocks.
///
/// This commits directly through the finalized state (as the disk writer does);
/// the trailing RPC indexer thread is not spawned on this path, so when the
/// split is on the RPC index database is populated separately by the test.
fn commit_blocks(
    config: &Config,
    network: &Network,
    max_height: u32,
) -> (FinalizedState, Vec<Arc<Block>>) {
    let mut state = FinalizedState::new_with_debug(
        config,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );

    let mut blocks = Vec::new();
    for height in 0..=max_height {
        let block: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .get(&height)
            .unwrap_or_else(|| panic!("continuous mainnet block {height} exists"))
            .zcash_deserialize_into()
            .expect("block deserializes");

        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "rpc-index split test commit",
            )
            .unwrap_or_else(|error| panic!("block {height} commits: {error:?}"));

        blocks.push(block);
    }

    (state, blocks)
}

/// Indexes every committed block into the split state's RPC index database, in
/// height order, exactly as the trailing indexer thread would (catching up from
/// the RPC index tip to the consensus tip).
fn run_indexer_to_tip(state: &FinalizedState, network: &Network) {
    let consensus_db = &state.db;
    let rpc_index_db = consensus_db
        .rpc_index_db()
        .expect("split state has an RPC index database");

    let start = rpc_index_db
        .rpc_index_tip_height()
        .map(|height| (height + 1).expect("valid height"))
        .unwrap_or(Height(0));

    let tip = consensus_db
        .finalized_tip_height()
        .expect("consensus database has a tip");

    let mut height = start;
    while height <= tip {
        let block = consensus_db
            .block(height.into())
            .expect("consensus database has the block");
        let hash = consensus_db
            .hash(height)
            .expect("consensus database has the hash");
        let finalized = crate::request::FinalizedBlock::for_rpc_index(block, hash);

        rpc_index_db
            .write_rpc_index_block(consensus_db, &finalized, network)
            .expect("RPC index block writes");

        height = (height + 1).expect("valid height");
    }
}

/// The transparent addresses that appear as a coinbase / output recipient in
/// `blocks`, so the parity test queries addresses that actually have balances.
fn addresses_in_blocks(blocks: &[Arc<Block>], network: &Network) -> HashSet<transparent::Address> {
    blocks
        .iter()
        .flat_map(|block| block.transactions.iter())
        .flat_map(|tx| tx.outputs().iter())
        .filter_map(|output| output.address(network))
        .collect()
}

/// With the split on, the consensus database is opened without the RPC-only
/// column families, and querying them routes to the RPC index database.
#[test]
fn split_consensus_db_has_no_rpc_only_cfs() {
    let _init_guard = zebra_test::init();

    // The consensus column families and the RPC-only column families are
    // disjoint (the RPC-index tip marker excepted, which is RPC-only).
    for rpc_cf in RPC_INDEX_COLUMN_FAMILIES_IN_CODE {
        assert!(
            !CONSENSUS_COLUMN_FAMILIES_IN_CODE.contains(rpc_cf),
            "RPC-only column family {rpc_cf} must not be in the consensus column families",
        );
    }

    let network = Network::Mainnet;
    let (state, _blocks) = commit_blocks(&split_config(), &network, 5);

    // The consensus database's own handle does not have the RPC-only column
    // families (it was opened without them).
    for rpc_cf in RPC_INDEX_COLUMN_FAMILIES_IN_CODE {
        assert!(
            state.db.raw_cf_handle_is_none_for_test(rpc_cf),
            "consensus database must not contain the RPC-only column family {rpc_cf}",
        );
    }

    // The RPC index database has them.
    let rpc_index_db = state.db.rpc_index_db().expect("split has an RPC index db");
    for rpc_cf in RPC_INDEX_COLUMN_FAMILIES_IN_CODE {
        assert!(
            !rpc_index_db.raw_cf_handle_is_none_for_test(rpc_cf),
            "RPC index database must contain the RPC-only column family {rpc_cf}",
        );
    }
}

/// Once the trailing indexer catches up, the RPC-only read accessors on a split
/// state return the same results as a single-database baseline.
#[test]
fn split_rpc_reads_match_single_db() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let max_height = 9;

    // Baseline: a single-database state (split off).
    let (baseline_state, blocks) = commit_blocks(&Config::ephemeral(), &network, max_height);

    // Split: a consensus + RPC-index state. Index every committed block.
    let (split_state, _split_blocks) = commit_blocks(&split_config(), &network, max_height);
    run_indexer_to_tip(&split_state, &network);

    let rpc_index_tip = split_state
        .db
        .rpc_index_db()
        .expect("split has an RPC index db")
        .rpc_index_tip_height()
        .expect("RPC index has a tip after catch-up");
    assert_eq!(
        rpc_index_tip,
        Height(max_height),
        "the RPC index caught up to the consensus tip",
    );

    let addresses = addresses_in_blocks(&blocks, &network);
    assert!(
        !addresses.is_empty(),
        "the test blocks must have transparent recipients to query",
    );

    for address in &addresses {
        // Balances match.
        assert_eq!(
            split_state.db.address_balance(address),
            baseline_state.db.address_balance(address),
            "split and single-db balances match for {address:?}",
        );

        // UTXOs match.
        assert_eq!(
            split_state.db.address_utxos(address),
            baseline_state.db.address_utxos(address),
            "split and single-db UTXOs match for {address:?}",
        );

        // Transaction ids match across the whole committed range.
        assert_eq!(
            split_state
                .db
                .address_tx_ids(address, Height(0)..=Height(max_height)),
            baseline_state
                .db
                .address_tx_ids(address, Height(0)..=Height(max_height)),
            "split and single-db address tx ids match for {address:?}",
        );
    }
}

/// A partially-indexed RPC index database (behind the consensus tip) is replayed
/// forward up to the consensus tip from the consensus database, reaching the
/// same result as a fully-indexed one.
#[test]
fn split_rpc_index_catches_up_from_behind() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let max_height = 9;

    let (split_state, blocks) = commit_blocks(&split_config(), &network, max_height);

    // Index only the first half of the chain, simulating a crash that left the
    // RPC index behind the consensus tip.
    {
        let consensus_db = &split_state.db;
        let rpc_index_db = consensus_db
            .rpc_index_db()
            .expect("split has an RPC index db");
        for height in 0..=(max_height / 2) {
            let block = consensus_db
                .block(Height(height).into())
                .expect("block exists");
            let hash = consensus_db.hash(Height(height)).expect("hash exists");
            let finalized = crate::request::FinalizedBlock::for_rpc_index(block, hash);
            rpc_index_db
                .write_rpc_index_block(consensus_db, &finalized, &network)
                .expect("partial index writes");
        }

        assert_eq!(
            rpc_index_db.rpc_index_tip_height(),
            Some(Height(max_height / 2)),
            "the RPC index is behind the consensus tip before catch-up",
        );
        assert!(
            rpc_index_db.rpc_index_tip_height().unwrap()
                < consensus_db.finalized_tip_height().unwrap(),
            "the RPC index never exceeds the consensus tip",
        );
    }

    // Catch up the rest of the way.
    run_indexer_to_tip(&split_state, &network);

    assert_eq!(
        split_state
            .db
            .rpc_index_db()
            .unwrap()
            .rpc_index_tip_height(),
        Some(Height(max_height)),
        "catch-up brought the RPC index to the consensus tip",
    );

    // The caught-up index answers queries for addresses across the whole range.
    let addresses = addresses_in_blocks(&blocks, &network);
    for address in &addresses {
        // Every queried address that received funds has a balance after catch-up.
        assert!(
            split_state.db.address_balance(address).is_some(),
            "address {address:?} has a balance after catch-up",
        );
    }
}

/// The disk-tip atomic the trailing indexer trails advances monotonically and
/// matches the consensus tip after each commit (sanity check of the atomic the
/// indexer reads).
#[test]
fn split_disk_tip_atomic_tracks_consensus_tip() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (state, _blocks) = commit_blocks(&split_config(), &network, 5);

    // Simulate the disk writer's published tip: it equals the consensus tip
    // after each durable commit. The indexer never indexes above this.
    let disk_tip = Arc::new(AtomicU32::new(
        state.db.finalized_tip_height().expect("tip").0,
    ));
    assert_eq!(
        disk_tip.load(Ordering::Acquire),
        state.db.finalized_tip_height().unwrap().0,
        "the durable-tip atomic equals the consensus tip",
    );
}

/// Builds a synthetic block at `height` with a coinbase transaction at index 0
/// (so `coinbase_height()` works, as it does for every real block) and one
/// payload transaction at index 1 that spends `spends` (earlier outputs) and
/// creates `output_values` to `address`.
///
/// The created outputs are therefore at `(height, transaction_index = 1,
/// output_index)`. Returns the [`FinalizedBlock`] and the maps `write_block`
/// builds for the transparent batch (the coinbase's value is ignored: it sends
/// to no address here, so it never enters the transparent index).
#[allow(clippy::type_complexity)]
fn synthetic_block(
    height: Height,
    address: &Address,
    output_values: &[Amount<NonNegative>],
    spends: &[(OutPoint, OutputLocation, Amount<NonNegative>)],
) -> (
    FinalizedBlock,
    BTreeMap<OutputLocation, transparent::Utxo>,
    HashMap<OutPoint, transparent::Utxo>,
    BTreeMap<OutputLocation, transparent::Utxo>,
    HashMap<OutPoint, OutputLocation>,
) {
    // The coinbase transaction at index 0, carrying the block height. Its output
    // pays an empty script (no transparent address), so it never appears in the
    // transparent index — keeping the parity assertions focused on the payload tx.
    let coinbase = Arc::new(Transaction::V1 {
        inputs: vec![Input::Coinbase {
            height,
            // The serialized coinbase script (height + data) must be at least
            // `MIN_COINBASE_SCRIPT_LEN` bytes, so pad with arbitrary data.
            data: vec![0u8; 8],
            sequence: 0xffff_ffff,
        }],
        outputs: vec![Output::new(Amount::zero(), Script::new(&[]))],
        lock_time: LockTime::unlocked(),
    });

    // The payload transaction at index 1.
    let inputs: Vec<Input> = spends
        .iter()
        .map(|(outpoint, _loc, _value)| Input::PrevOut {
            outpoint: *outpoint,
            unlock_script: Script::new(&[]),
            sequence: 0xffff_ffff,
        })
        .collect();
    let outputs: Vec<Output> = output_values
        .iter()
        .map(|value| Output::new(*value, address.script()))
        .collect();

    let payload = Arc::new(Transaction::V1 {
        inputs,
        outputs,
        lock_time: LockTime::unlocked(),
    });

    let header: block::Header = zebra_test::vectors::DUMMY_HEADER
        .as_slice()
        .zcash_deserialize_into()
        .expect("DUMMY_HEADER deserializes");
    let block = Arc::new(Block {
        header: Arc::new(header),
        transactions: vec![coinbase, payload],
    });
    let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let new_outputs = new_ordered_outputs_with_height(&block, height, &transaction_hashes);

    let semantically_verified = SemanticallyVerifiedBlock {
        block: block.clone(),
        hash: block::Hash([height.0 as u8; 32]),
        height,
        new_outputs,
        transaction_hashes,
    };
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock(semantically_verified),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );

    // The payload transaction is at index 1 in the block.
    let new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = (0..output_values
        .len())
        .map(|output_index| {
            let loc = OutputLocation::from_usize(height, 1, output_index);
            let utxo = transparent::Utxo::new(
                Output::new(output_values[output_index], address.script()),
                height,
                false,
            );
            (loc, utxo)
        })
        .collect();

    let mut spent_utxos_by_outpoint: HashMap<OutPoint, transparent::Utxo> = HashMap::new();
    let mut spent_utxos_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = BTreeMap::new();
    let mut out_loc_by_outpoint: HashMap<OutPoint, OutputLocation> = HashMap::new();
    for (outpoint, loc, value) in spends {
        let utxo =
            transparent::Utxo::new(Output::new(*value, address.script()), loc.height(), false);
        spent_utxos_by_outpoint.insert(*outpoint, utxo.clone());
        spent_utxos_by_out_loc.insert(*loc, utxo);
        out_loc_by_outpoint.insert(*outpoint, *loc);
    }

    (
        finalized,
        new_outputs_by_out_loc,
        spent_utxos_by_outpoint,
        spent_utxos_by_out_loc,
        out_loc_by_outpoint,
    )
}

/// Commits one synthetic block to `db`'s consensus column families exactly as the
/// consensus portion of `write_block` would, using `kind` to select whether the
/// RPC-only indexes are written inline (`Combined`, single-database) or skipped
/// (`ConsensusOnly`, the split's consensus database).
///
/// This writes the block header / transaction data (so `tx_by_loc` /
/// `tx_loc_by_hash` resolve later) and the transparent UTXO set, deleting spent
/// outputs from `utxo_by_out_loc` just as a real commit does — the deletion that
/// the trailing indexer's spend resolution must not depend on.
fn commit_consensus_block(
    db: &ZebraDb,
    network: &Network,
    kind: TransparentBatchKind,
    height: Height,
    address: &Address,
    output_values: &[Amount<NonNegative>],
    spends: &[(OutPoint, OutputLocation, Amount<NonNegative>)],
) {
    let (
        finalized,
        new_outputs_by_out_loc,
        spent_utxos_by_outpoint,
        spent_utxos_by_out_loc,
        out_loc_by_outpoint,
    ) = synthetic_block(height, address, output_values, spends);

    // The address balances `write_block` reads before this block. Only needed
    // for the inline RPC-index path (`Combined`); skipped for `ConsensusOnly`.
    let address_balances = if kind.writes_rpc_indexes() {
        let mut balances = HashMap::new();
        if let Some(abl) = db.address_balance_location(address) {
            balances.insert(*address, abl);
        }
        AddressBalanceLocationUpdates::Insert(balances)
    } else {
        AddressBalanceLocationUpdates::Insert(HashMap::new())
    };

    let mut batch = DiskWriteBatch::new();
    batch.prepare_block_header_and_transaction_data_batch(db.disk_db_for_test(), &finalized);
    batch.prepare_transparent_transaction_batch_split(
        db,
        network,
        &finalized,
        &new_outputs_by_out_loc,
        &spent_utxos_by_outpoint,
        &spent_utxos_by_out_loc,
        &out_loc_by_outpoint,
        address_balances,
        kind,
    );
    db.write_batch(batch)
        .expect("ephemeral db accepts the batch");
}

/// A chain with a cross-block transparent spend: split RPC reads, once the
/// trailing indexer catches up, are byte-identical to a single-database write of
/// the same chain.
///
/// Block 1 creates two outputs to address A. Block 2 spends output 0 (created in
/// block 1) — a *cross-block* spend — and creates a new output to A. The
/// consensus commit deletes block 1's spent output from `utxo_by_out_loc`, so the
/// trailing indexer must resolve that spend's value from the transaction body
/// (`tx_by_loc`), not the deleted unspent set. If it resolved from the unspent
/// set, the spend's debit and spending-tx index would be dropped and the split
/// balance / UTXO set / tx-ids would diverge from the single-database path.
#[test]
fn split_rpc_reads_match_single_db_with_cross_block_spend() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]);
    let value = Amount::<NonNegative>::try_from(10_000).expect("fits in Amount");

    // Block 1's two created outputs (the payload transaction is at index 1).
    let created_0 = OutputLocation::from_usize(Height(1), 1, 0);
    let created_1 = OutputLocation::from_usize(Height(1), 1, 1);

    // Block 1's payload transaction hash, so block 2 can spend its output.
    let (block1_finalized, _, _, _, _) = synthetic_block(Height(1), &address, &[value, value], &[]);
    let block1_tx_hash = block1_finalized.block.transactions[1].hash();
    // Block 2 spends output 0 of block 1's payload transaction (a cross-block spend).
    let cross_block_outpoint = OutPoint {
        hash: block1_tx_hash,
        index: 0,
    };
    let spend_value = value;

    // Baseline: a single-database state (split off), inline combined writes.
    let baseline_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );
    commit_consensus_block(
        &baseline_state.db,
        &network,
        TransparentBatchKind::Combined,
        Height(1),
        &address,
        &[value, value],
        &[],
    );
    commit_consensus_block(
        &baseline_state.db,
        &network,
        TransparentBatchKind::Combined,
        Height(2),
        &address,
        &[value],
        &[(cross_block_outpoint, created_0, spend_value)],
    );

    // Split: consensus database with only the consensus writes; the trailing
    // indexer then writes the RPC-only indexes to the RPC index database.
    let split_state = FinalizedState::new_with_debug(
        &split_config(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );
    commit_consensus_block(
        &split_state.db,
        &network,
        TransparentBatchKind::ConsensusOnly,
        Height(1),
        &address,
        &[value, value],
        &[],
    );
    commit_consensus_block(
        &split_state.db,
        &network,
        TransparentBatchKind::ConsensusOnly,
        Height(2),
        &address,
        &[value],
        &[(cross_block_outpoint, created_0, spend_value)],
    );

    // Sanity: the consensus commit deleted the cross-block spent output from the
    // consensus unspent set, so the indexer cannot read its value there.
    assert!(
        split_state.db.utxo_by_location(created_0).is_none(),
        "the cross-block spent output is deleted from the consensus unspent set",
    );
    // But the transaction body (the indexer's resolution source) is still there.
    assert!(
        split_state.db.output_by_location(&created_0).is_some(),
        "the spent output's value is still resolvable from the transaction body",
    );

    // Index both heights into the RPC index database (the trailing indexer).
    let consensus_db = &split_state.db;
    let rpc_index_db = consensus_db
        .rpc_index_db()
        .expect("split state has an RPC index database");
    for height in 1..=2u32 {
        let block = consensus_db
            .block(Height(height).into())
            .expect("consensus database has the block");
        let hash = consensus_db.hash(Height(height)).expect("hash exists");
        let finalized = FinalizedBlock::for_rpc_index(block, hash);
        rpc_index_db
            .write_rpc_index_block(consensus_db, &finalized, &network)
            .expect("RPC index block writes");
    }

    // The split balance must equal the single-database balance: value
    // (received 3 * value across three created outputs, spendable 2 * value
    // after the one cross-block spend). Reading from the deleted unspent set
    // would have dropped the spend's debit and produced a larger balance.
    assert_eq!(
        split_state.db.address_balance(&address),
        baseline_state.db.address_balance(&address),
        "split and single-db balances match across a cross-block spend",
    );

    // The address UTXO set must match (created_0 spent; created_1 and the
    // block-2 output unspent).
    assert_eq!(
        split_state.db.address_utxos(&address),
        baseline_state.db.address_utxos(&address),
        "split and single-db UTXOs match across a cross-block spend",
    );
    assert!(
        split_state
            .db
            .address_utxos(&address)
            .keys()
            .all(|loc| *loc != created_0),
        "the cross-block spent output is not in the address UTXO set",
    );

    // The address transaction ids (which include the spending transaction) must
    // match across the whole range.
    assert_eq!(
        split_state
            .db
            .address_tx_ids(&address, Height(0)..=Height(2)),
        baseline_state
            .db
            .address_tx_ids(&address, Height(0)..=Height(2)),
        "split and single-db address tx ids match across a cross-block spend",
    );

    // And the spend itself is indexed: created_1 is unspent, but created_0 was
    // spent in block 2's transaction, which both databases must record.
    let _ = created_1;
}

/// Under the split, the bulk address-balance writer and the bulk readers all
/// target the RPC index database, not the consensus database.
///
/// `bulk_load_address_balances` (used by snapshot-consume to load the final
/// `H_max` balances), `all_address_balances`, `for_each_address_balance_bytes`,
/// and `address_balance` must all use the database the
/// `balance_by_transparent_addr` column-family handle belongs to. A column-family
/// handle is bound to its database, so routing a write or iteration to the wrong
/// one is silently incorrect; this round-trip would not survive that.
#[test]
fn split_bulk_address_balances_round_trip_via_rpc_index_db() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let state = FinalizedState::new_with_debug(
        &split_config(),
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );

    // Two synthetic final balances to bulk-load.
    let address_a = Address::from_script_hash(NetworkKind::Mainnet, [0x01; 20]);
    let address_b = Address::from_script_hash(NetworkKind::Mainnet, [0x02; 20]);
    let loc_a = OutputLocation::from_usize(Height(1), 0, 0);
    let loc_b = OutputLocation::from_usize(Height(2), 0, 0);

    let mut abl_a = AddressBalanceLocation::new(loc_a);
    *abl_a.balance_mut() = Amount::try_from(1_000).expect("fits");
    *abl_a.received_mut() = 1_000;
    let mut abl_b = AddressBalanceLocation::new(loc_b);
    *abl_b.balance_mut() = Amount::try_from(2_000).expect("fits");
    *abl_b.received_mut() = 2_000;

    state
        .db
        .bulk_load_address_balances([(address_a, abl_a), (address_b, abl_b)], 1)
        .expect("bulk load succeeds");

    // The consensus database must not have the balance column family at all (it
    // was opened without it), so the bulk write could only have gone to the RPC
    // index database.
    assert!(
        state.db.raw_cf_handle_is_none_for_test(
            crate::service::finalized_state::zebra_db::transparent::BALANCE_BY_TRANSPARENT_ADDR
        ),
        "the consensus database has no balance column family under the split",
    );

    // The bulk-loaded balances read back identically through every accessor.
    assert_eq!(
        state.db.address_balance(&address_a),
        Some((Amount::try_from(1_000).expect("fits"), 1_000)),
        "address A balance reads back from the RPC index database",
    );
    assert_eq!(
        state.db.address_balance(&address_b),
        Some((Amount::try_from(2_000).expect("fits"), 2_000)),
        "address B balance reads back from the RPC index database",
    );

    let all = state.db.all_address_balances();
    assert_eq!(all.len(), 2, "both bulk-loaded balances are present");
    let loaded_addresses: HashSet<_> = all.iter().map(|(address, _)| *address).collect();
    assert!(
        loaded_addresses.contains(&address_a) && loaded_addresses.contains(&address_b),
        "both bulk-loaded addresses read back from the RPC index database",
    );

    // The streaming byte accessor sees the same two records.
    let mut streamed = 0usize;
    state
        .db
        .for_each_address_balance_bytes(|_key, _value| streamed += 1);
    assert_eq!(
        streamed, 2,
        "the byte stream sees both bulk-loaded balances"
    );
}
