//! Fixed-vector tests for the finalized transparent address balance writer.
//!
//! Regression coverage for the credit-before-debit overflow panic: applying a
//! same-address transparent self-spend chain in one block used to push the
//! intermediate address balance above `MAX_MONEY` and panic the writer, even
//! though the final consensus balance was valid.
//!
//! This test drives `DiskWriteBatch::prepare_transparent_transaction_batch`
//! (the writer's public entry point, whose signature is unchanged by the fix)
//! so the same source compiles against both the buggy and the fixed revision:
//! it panics on the buggy revision and passes on the fixed one.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative, MAX_MONEY},
    block::{self, Block, Height},
    parameters::{Network, NetworkKind},
    serialization::ZcashDeserializeInto,
    transaction::{self, LockTime, Transaction},
    transparent::{
        self, new_ordered_outputs_with_height, Address, Input, OutPoint, Output, Script,
    },
};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    request::{FinalizedBlock, SemanticallyVerifiedBlock, Treestate},
    service::finalized_state::{
        disk_db::DiskWriteBatch,
        disk_format::transparent::{
            AddressBalanceLocation, AddressBalanceLocationUpdates, OutputLocation,
        },
        IntoDisk, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    snapshot_consume::SnapshotConsumeConfig,
    CheckpointVerifiedBlock, Config,
};

fn new_ephemeral_zebra_db(network: &Network) -> ZebraDb {
    new_ephemeral_zebra_db_with_config(network, &Config::ephemeral())
}

fn new_ephemeral_zebra_db_with_config(network: &Network, config: &Config) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        // The raw database accesses in this test create invalid database formats.
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
}

/// Collects the 8-byte on-disk output locations of every unspent transparent
/// output, for parity comparisons.
fn unspent_output_locations(db: &ZebraDb) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    db.for_each_unspent_output_location_bytes(|bytes| out.push(bytes.to_vec()));
    out
}

/// Writes `locations` (as 8-byte on-disk records) to a temp file and returns a
/// `Config` with snapshot-consume pointing at it.
///
/// The temp file's directory is leaked into the returned guard so it outlives
/// the test's database use.
fn config_with_survivor_set(
    survivor_locations: &[OutputLocation],
    h_max: u32,
    elide_utxo_bytes: bool,
) -> (Config, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir is created");
    let path = dir.path().join("survivors.bin");

    // The survivor set must be sorted ascending by on-disk bytes.
    let mut sorted: Vec<[u8; 8]> = survivor_locations
        .iter()
        .map(|loc| loc.as_bytes())
        .collect();
    sorted.sort_unstable();
    let bytes: Vec<u8> = sorted.iter().flatten().copied().collect();
    std::fs::write(&path, bytes).expect("survivor set file is written");

    let config = Config {
        snapshot_consume: Some(SnapshotConsumeConfig {
            survivor_set_path: Some(path),
            h_max,
            elide_utxo_bytes,
        }),
        ..Config::ephemeral()
    };

    (config, dir)
}

/// Cross-version regression test for the credit-before-debit overflow panic.
///
/// Builds a synthetic block whose two transactions form a same-address self-spend
/// chain that re-creates an existing `MAX_MONEY / 2` UTXO twice, and drives it
/// through [`DiskWriteBatch::prepare_transparent_transaction_batch`].
///
/// - On the **buggy** revision the writer credits both new outputs before debiting
///   the matching spends, so the intermediate balance reaches `1.5 * MAX_MONEY`
///   and panics with `"balance overflow already checked"`.
/// - On the **fixed** revision the writer processes each transaction in block order
///   (debit-then-credit), so the per-address running balance stays in
///   `[0, MAX_MONEY]` and the final on-disk balance equals the original
///   `MAX_MONEY / 2`.
#[test]
fn intra_block_self_spend_chain_in_finalized_state() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let height = Height(1);
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]);
    let value = Amount::<NonNegative>::try_from(MAX_MONEY / 2)
        .expect("MAX_MONEY / 2 fits in Amount<NonNegative>");

    // T0 spends a pre-existing on-chain UTXO of value V to address A and re-creates V to A.
    let existing_outpoint = OutPoint {
        hash: transaction::Hash([0x00; 32]),
        index: 0,
    };
    let t0 = Arc::new(Transaction::V1 {
        inputs: vec![Input::PrevOut {
            outpoint: existing_outpoint,
            unlock_script: Script::new(&[]),
            sequence: 0xffff_ffff,
        }],
        outputs: vec![Output::new(value, address.script())],
        lock_time: LockTime::unlocked(),
    });
    let t0_hash = t0.hash();

    // T1 spends T0's output and creates V back to A.
    let t0_output_outpoint = OutPoint {
        hash: t0_hash,
        index: 0,
    };
    let t1 = Arc::new(Transaction::V1 {
        inputs: vec![Input::PrevOut {
            outpoint: t0_output_outpoint,
            unlock_script: Script::new(&[]),
            sequence: 0xffff_ffff,
        }],
        outputs: vec![Output::new(value, address.script())],
        lock_time: LockTime::unlocked(),
    });

    // Synthetic block. The header is a dummy from zebra-test (round-tripped); the writer
    // doesn't validate the header — only the transactions and the block height matter.
    let header: block::Header = zebra_test::vectors::DUMMY_HEADER
        .as_slice()
        .zcash_deserialize_into()
        .expect("DUMMY_HEADER deserializes");
    let block = Arc::new(Block {
        header: Arc::new(header),
        transactions: vec![t0.clone(), t1.clone()],
    });
    let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let new_outputs = new_ordered_outputs_with_height(&block, height, &transaction_hashes);

    let semantically_verified = SemanticallyVerifiedBlock {
        block: block.clone(),
        hash: block::Hash([0x00; 32]),
        height,
        new_outputs,
        transaction_hashes,
    };
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock(semantically_verified, None),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );

    // Inputs to `prepare_transparent_transaction_batch`, prepared the way `write_block` does.
    let existing_output_location = OutputLocation::from_usize(Height(0), 0, 0);
    let t0_output_location = OutputLocation::from_usize(height, 0, 0);
    let t1_output_location = OutputLocation::from_usize(height, 1, 0);

    let make_utxo =
        |h: Height| transparent::Utxo::new(Output::new(value, address.script()), h, false);
    let existing_utxo = make_utxo(Height(0));
    let t0_output_utxo = make_utxo(height);
    let t1_output_utxo = make_utxo(height);

    let new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = BTreeMap::from([
        (t0_output_location, t0_output_utxo.clone()),
        (t1_output_location, t1_output_utxo),
    ]);
    let spent_utxos_by_outpoint: HashMap<OutPoint, transparent::Utxo> = HashMap::from([
        (existing_outpoint, existing_utxo.clone()),
        (t0_output_outpoint, t0_output_utxo.clone()),
    ]);
    let spent_utxos_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = BTreeMap::from([
        (existing_output_location, existing_utxo),
        (t0_output_location, t0_output_utxo),
    ]);

    // Pre-populate `address_balances` with A's pre-block on-chain balance, the way
    // `block.rs` does via `read_addr_locs`.
    let mut existing_abl = AddressBalanceLocation::new(existing_output_location);
    *existing_abl.balance_mut() = value;
    *existing_abl.received_mut() = u64::from(value);
    let address_balances =
        AddressBalanceLocationUpdates::Insert(HashMap::from([(address, existing_abl)]));

    let zebra_db = new_ephemeral_zebra_db(&network);
    let mut batch = DiskWriteBatch::new();

    // On the buggy revision this call panics with "balance overflow already checked" during
    // the credit-first batch (intermediate balance reaches 1.5 * MAX_MONEY). On the fixed
    // revision it completes cleanly.
    // The map from spent outpoints to their output locations, as `write_block`
    // builds it (here: the existing UTXO and T0's output).
    let out_loc_by_outpoint: HashMap<OutPoint, OutputLocation> = HashMap::from([
        (existing_outpoint, existing_output_location),
        (t0_output_outpoint, t0_output_location),
    ]);

    batch.prepare_transparent_transaction_batch(
        &zebra_db,
        &network,
        &finalized,
        &new_outputs_by_out_loc,
        &spent_utxos_by_outpoint,
        &spent_utxos_by_out_loc,
        &out_loc_by_outpoint,
        address_balances,
    );

    // Write the batch and confirm the final on-disk balance matches the consensus value
    // (existing V − 2*V debits + 2*V credits = V).
    zebra_db
        .write_batch(batch)
        .expect("ephemeral db accepts the batch");

    let (balance, received) = zebra_db
        .address_balance(&address)
        .expect("address balance is present after writing the batch");
    assert_eq!(balance, value, "final balance equals the existing balance");
    assert_eq!(
        received,
        u64::from(value).saturating_mul(3),
        "received counts the existing V plus two intra-block credits of V",
    );
}

/// Builds a `FinalizedBlock` and the `write_block`-shaped inputs for a block at
/// `height` whose single non-coinbase transaction creates `outputs` to `address`
/// and spends `spends` (outpoint, location, value) of earlier outputs.
///
/// Returns `(finalized, new_outputs_by_out_loc, spent_utxos_by_outpoint,
/// spent_utxos_by_out_loc, out_loc_by_outpoint, created_output_locations)`.
#[allow(clippy::type_complexity)]
fn build_block_batch_inputs(
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
    Vec<OutputLocation>,
) {
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

    let tx = Arc::new(Transaction::V1 {
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
        transactions: vec![tx.clone()],
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
        CheckpointVerifiedBlock(semantically_verified, None),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );

    // The single tx is at index 0 in the block.
    let created_output_locations: Vec<OutputLocation> = (0..output_values.len())
        .map(|output_index| OutputLocation::from_usize(height, 0, output_index))
        .collect();
    let new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> =
        created_output_locations
            .iter()
            .zip(output_values.iter())
            .map(|(loc, value)| {
                (
                    *loc,
                    transparent::Utxo::new(Output::new(*value, address.script()), height, false),
                )
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
        created_output_locations,
    )
}

/// Writes one block's transparent batch to `db`, reading the address balance the
/// way `write_block` does (insert variant), and returns the created output
/// locations.
fn write_block_transparent_batch(
    db: &ZebraDb,
    network: &Network,
    address: &Address,
    height: Height,
    output_values: &[Amount<NonNegative>],
    spends: &[(OutPoint, OutputLocation, Amount<NonNegative>)],
) -> Vec<OutputLocation> {
    let (
        finalized,
        new_outputs_by_out_loc,
        spent_utxos_by_outpoint,
        spent_utxos_by_out_loc,
        out_loc_by_outpoint,
        created_output_locations,
    ) = build_block_batch_inputs(height, address, output_values, spends);

    // Read the current on-disk balance for the address (the only changed one),
    // as `write_block` does.
    let mut balances = HashMap::new();
    if let Some(abl) = db.address_balance_location(address) {
        balances.insert(*address, abl);
    }
    let address_balances = AddressBalanceLocationUpdates::Insert(balances);

    let mut batch = DiskWriteBatch::new();
    batch.prepare_transparent_transaction_batch(
        db,
        network,
        &finalized,
        &new_outputs_by_out_loc,
        &spent_utxos_by_outpoint,
        &spent_utxos_by_out_loc,
        &out_loc_by_outpoint,
        address_balances,
    );
    db.write_batch(batch)
        .expect("ephemeral db accepts the batch");

    created_output_locations
}

/// `H_max` UTXO-set parity: a create+spend fixture written with survivor-only
/// elision (in the unsafe UTXO-byte mode) produces the **same** final
/// `utxo_by_out_loc` set as a non-elided write.
///
/// Block 1 creates two outputs to address A (output 0 survives, output 1 is
/// spent in block 2). At `H_max = 2` only output 0 is unspent. Eliding the
/// non-survivor's create and spend nets to zero, so the survivor set on disk is
/// byte-identical to the non-elided write.
#[test]
fn h_max_utxo_set_parity_with_survivor_elision() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]);
    let value = Amount::<NonNegative>::try_from(1_000).expect("fits in Amount");

    // Block 1 creates two outputs at (height 1, tx 0, output 0) and (.., output 1).
    let survivor_loc = OutputLocation::from_usize(Height(1), 0, 0);
    let non_survivor_loc = OutputLocation::from_usize(Height(1), 0, 1);

    // Baseline (no elision).
    let baseline_db = new_ephemeral_zebra_db(&network);
    write_block_transparent_batch(
        &baseline_db,
        &network,
        &address,
        Height(1),
        &[value, value],
        &[],
    );
    // Block 2 spends the non-survivor (height 1, tx 0, output 1).
    let non_survivor_outpoint = OutPoint {
        hash: transaction::Hash([0x11; 32]),
        index: 1,
    };
    write_block_transparent_batch(
        &baseline_db,
        &network,
        &address,
        Height(2),
        &[],
        &[(non_survivor_outpoint, non_survivor_loc, value)],
    );
    let baseline_utxos = unspent_output_locations(&baseline_db);

    // Elided run: survivor set lists only the survivor; unsafe UTXO-byte elision
    // on, so the non-survivor's create+spend UTXO writes are both elided.
    let (config, _dir) = config_with_survivor_set(&[survivor_loc], 2, true);
    let elided_db = new_ephemeral_zebra_db_with_config(&network, &config);
    write_block_transparent_batch(
        &elided_db,
        &network,
        &address,
        Height(1),
        &[value, value],
        &[],
    );
    write_block_transparent_batch(
        &elided_db,
        &network,
        &address,
        Height(2),
        &[],
        &[(non_survivor_outpoint, non_survivor_loc, value)],
    );
    let elided_utxos = unspent_output_locations(&elided_db);

    assert_eq!(
        baseline_utxos, elided_utxos,
        "the H_max UTXO set must be byte-identical with and without elision",
    );
    // Sanity: exactly the survivor remains.
    assert_eq!(elided_utxos, vec![survivor_loc.as_bytes().to_vec()]);
    assert!(
        elided_db.utxo_by_location(survivor_loc).is_some(),
        "the survivor's UTXO bytes are written",
    );
    assert!(
        elided_db.utxo_by_location(non_survivor_loc).is_none(),
        "the non-survivor's UTXO bytes are net-zero (create+spend both elided)",
    );
}

/// Balance net-zero: eliding a non-survivor's create-credit and its matching
/// spend-debit together keeps the address balance equal to the non-elided
/// balance at `H_max`.
#[test]
fn balance_net_zero_with_survivor_elision() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let address = Address::from_script_hash(NetworkKind::Mainnet, [0x07; 20]);
    let value = Amount::<NonNegative>::try_from(5_000).expect("fits in Amount");

    let survivor_loc = OutputLocation::from_usize(Height(1), 0, 0);
    let non_survivor_loc = OutputLocation::from_usize(Height(1), 0, 1);
    let non_survivor_outpoint = OutPoint {
        hash: transaction::Hash([0x22; 32]),
        index: 1,
    };

    // Baseline.
    let baseline_db = new_ephemeral_zebra_db(&network);
    write_block_transparent_batch(
        &baseline_db,
        &network,
        &address,
        Height(1),
        &[value, value],
        &[],
    );
    write_block_transparent_batch(
        &baseline_db,
        &network,
        &address,
        Height(2),
        &[],
        &[(non_survivor_outpoint, non_survivor_loc, value)],
    );
    let baseline_balance = baseline_db.address_balance(&address).expect("balance");

    // Elided (safe default: address indexes + balance elided, UTXO bytes kept).
    let (config, _dir) = config_with_survivor_set(&[survivor_loc], 2, false);
    let elided_db = new_ephemeral_zebra_db_with_config(&network, &config);
    write_block_transparent_batch(
        &elided_db,
        &network,
        &address,
        Height(1),
        &[value, value],
        &[],
    );
    write_block_transparent_batch(
        &elided_db,
        &network,
        &address,
        Height(2),
        &[],
        &[(non_survivor_outpoint, non_survivor_loc, value)],
    );
    let elided_balance = elided_db.address_balance(&address).expect("balance");

    // The spendable balance matches at H_max (both equal the survivor's value);
    // the elided run never counted the non-survivor.
    assert_eq!(
        baseline_balance.0, elided_balance.0,
        "spendable balance is identical at H_max",
    );
    assert_eq!(
        baseline_balance.0, value,
        "only the survivor's value remains spendable",
    );
    // With the safe default, the UTXO set is unaffected by balance elision.
    assert_eq!(
        unspent_output_locations(&baseline_db),
        unspent_output_locations(&elided_db),
        "UTXO set is unchanged by address-index/balance elision",
    );
}
