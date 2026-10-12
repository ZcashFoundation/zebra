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
        ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    CheckpointVerifiedBlock, Config,
};

fn new_ephemeral_zebra_db(network: &Network) -> ZebraDb {
    ZebraDb::new(
        &Config::ephemeral(),
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
    .expect("opening an ephemeral database should succeed")
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
    let t0 = Arc::new(Transaction::test_v1(
        vec![Input::PrevOut {
            outpoint: existing_outpoint,
            unlock_script: Script::new(&[]),
            sequence: 0xffff_ffff,
        }],
        vec![Output::new(value, address.script())],
        LockTime::unlocked(),
    ));
    let t0_hash = t0.hash();

    // T1 spends T0's output and creates V back to A.
    let t0_output_outpoint = OutPoint {
        hash: t0_hash,
        index: 0,
    };
    let t1 = Arc::new(Transaction::test_v1(
        vec![Input::PrevOut {
            outpoint: t0_output_outpoint,
            unlock_script: Script::new(&[]),
            sequence: 0xffff_ffff,
        }],
        vec![Output::new(value, address.script())],
        LockTime::unlocked(),
    ));

    // Synthetic block. The header is a dummy from zebra-test (round-tripped); the writer
    // doesn't validate the header — only the transactions and the block height matter.
    let header: block::Header = zebra_test::vectors::DUMMY_HEADER
        .as_slice()
        .zcash_deserialize_into()
        .expect("DUMMY_HEADER deserializes");
    let coinbase = Arc::new(Transaction::test_v1(
        vec![Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![],
        LockTime::unlocked(),
    ));
    let block = Arc::new(Block {
        header: Arc::new(header),
        transactions: vec![coinbase, t0.clone(), t1.clone()],
    });
    let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let new_outputs = new_ordered_outputs_with_height(&block, height, &transaction_hashes);

    let semantically_verified = SemanticallyVerifiedBlock {
        block: block.clone(),
        hash: block::Hash([0x00; 32]),
        height,
        new_outputs,
        transaction_hashes,
        received_time: None,
    };
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock::from(semantically_verified),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );

    // Inputs to `prepare_transparent_transaction_batch`, prepared the way `write_block` does.
    let existing_output_location = OutputLocation::from_usize(Height(0), 0, 0);
    let t0_output_location = OutputLocation::from_usize(height, 1, 0);
    let t1_output_location = OutputLocation::from_usize(height, 2, 0);

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
    batch.prepare_transparent_transaction_batch(
        &zebra_db,
        &network,
        &finalized,
        &new_outputs_by_out_loc,
        &spent_utxos_by_outpoint,
        &spent_utxos_by_out_loc,
        #[cfg(feature = "indexer")]
        &HashMap::from([
            (existing_outpoint, existing_output_location),
            (t0_output_outpoint, t0_output_location),
        ]),
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

/// Fee redistribution preserves balances and the first output location of a new address.
#[test]
fn coinbase_fees_preserve_finalized_balances_and_address_locations() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let height = Height(1);
    let existing_address = Address::from_script_hash(NetworkKind::Mainnet, [0x43; 20]);
    let new_address = Address::from_script_hash(NetworkKind::Mainnet, [0x44; 20]);
    let value = Amount::<NonNegative>::try_from(MAX_MONEY / 2 + 3).unwrap();
    let returned = Amount::<NonNegative>::try_from(MAX_MONEY / 2 + 1).unwrap();
    let one = Amount::<NonNegative>::try_from(1).unwrap();
    let existing_outpoint = OutPoint {
        hash: transaction::Hash([0x45; 32]),
        index: 0,
    };
    let existing_output_location = OutputLocation::from_usize(Height(0), 1, 0);
    let existing_utxo =
        transparent::OrderedUtxo::new(Output::new(value, existing_address.script()), Height(0), 1);
    let coinbase = Arc::new(Transaction::test_v1(
        vec![Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![
            Output::new(returned, existing_address.script()),
            Output::new(one, new_address.script()),
        ],
        LockTime::unlocked(),
    ));
    let spending = Arc::new(Transaction::test_v1(
        vec![Input::PrevOut {
            outpoint: existing_outpoint,
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        }],
        vec![Output::new(one, new_address.script())],
        LockTime::unlocked(),
    ));
    let block = Arc::new(Block {
        header: Arc::new(
            zebra_test::vectors::DUMMY_HEADER
                .zcash_deserialize_into()
                .unwrap(),
        ),
        transactions: vec![coinbase, spending],
    });
    assert_eq!(
        block
            .transaction_fees(&HashMap::from([(
                existing_outpoint,
                existing_utxo.utxo.clone()
            )]))
            .unwrap(),
        (returned + one).unwrap(),
    );
    let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let new_outputs = new_ordered_outputs_with_height(&block, height, &transaction_hashes);
    let new_outputs_by_out_loc = block
        .transactions
        .iter()
        .enumerate()
        .flat_map(|(tx_index, tx)| {
            tx.outputs()
                .into_iter()
                .enumerate()
                .map(move |(output_index, output)| {
                    (
                        OutputLocation::from_usize(height, tx_index, output_index),
                        transparent::Utxo::new(output, height, tx_index == 0),
                    )
                })
        })
        .collect();
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock(SemanticallyVerifiedBlock {
            hash: block.hash(),
            block,
            height,
            new_outputs,
            transaction_hashes,
            received_time: None,
        }),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );
    let mut balance = AddressBalanceLocation::new(existing_output_location);
    *balance.balance_mut() = value;
    *balance.received_mut() = u64::from(value);
    let zebra_db = new_ephemeral_zebra_db(&network);
    let mut batch = DiskWriteBatch::new();
    batch.prepare_transparent_transaction_batch(
        &zebra_db,
        &network,
        &finalized,
        &new_outputs_by_out_loc,
        &HashMap::from([(existing_outpoint, existing_utxo.utxo.clone())]),
        &BTreeMap::from([(existing_output_location, existing_utxo.utxo)]),
        #[cfg(feature = "indexer")]
        &HashMap::from([(existing_outpoint, existing_output_location)]),
        AddressBalanceLocationUpdates::Insert(HashMap::from([(existing_address, balance)])),
    );
    zebra_db.write_batch(batch).unwrap();
    assert_eq!(
        zebra_db.address_balance(&existing_address),
        Some((returned, u64::from(value) + u64::from(returned))),
    );
    assert_eq!(
        zebra_db.address_balance(&new_address),
        Some(((one + one).unwrap(), 2)),
    );
    assert_eq!(
        zebra_db.address_location(&new_address),
        Some(OutputLocation::from_usize(height, 0, 1)),
    );
}
