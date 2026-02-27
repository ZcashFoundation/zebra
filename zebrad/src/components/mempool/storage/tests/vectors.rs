//! Fixed test vectors for mempool storage.

#![allow(clippy::unwrap_in_result)]

use std::iter;

use color_eyre::eyre::Result;

use transparent::OutPoint;
use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Block, Height},
    parameters::Network,
};

use zebra_chain::transparent;

use crate::components::mempool::{storage::*, Mempool};

/// Eviction memory time used for tests. Most tests won't care about this
/// so we use a large enough value that will never be reached in the tests.
const EVICTION_MEMORY_TIME: Duration = Duration::from_secs(60 * 60);

/// Transaction count used in some tests to derive the mempool test size.
const MEMPOOL_TX_COUNT: usize = 4;

#[test]
fn mempool_storage_crud_exact_mainnet() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    // Get one (1) unmined transaction
    let unmined_tx = network
        .unmined_transactions_in_blocks(..)
        .next_back()
        .expect("at least one unmined transaction");

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx.clone(), Vec::new(), None);

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains_transaction_exact(&unmined_tx.transaction.id.mined_id()));

    // Remove tx
    let removal_count = storage.remove_exact(&iter::once(unmined_tx.transaction.id).collect());

    // Check that it is /not/ in the mempool.
    assert_eq!(removal_count, 1);
    assert!(!storage.contains_transaction_exact(&unmined_tx.transaction.id.mined_id()));
}

#[test]
fn mempool_storage_basic() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Test multiple times to catch intermittent bugs since eviction is randomized
    for _ in 0..10 {
        for network in Network::iter() {
            mempool_storage_basic_for_network(network)?;
        }
    }

    Ok(())
}

fn mempool_storage_basic_for_network(network: Network) -> Result<()> {
    // Get transactions from the first 10 blocks of the Zcash blockchain
    let unmined_transactions: Vec<_> = network.unmined_transactions_in_blocks(..=10).collect();

    assert!(
        MEMPOOL_TX_COUNT < unmined_transactions.len(),
        "inconsistent MEMPOOL_TX_COUNT value for this test; decrease it"
    );

    // Use the sum of the costs of the first `MEMPOOL_TX_COUNT` transactions
    // as the cost limit
    let tx_cost_limit = unmined_transactions
        .iter()
        .take(MEMPOOL_TX_COUNT)
        .map(|tx| tx.cost())
        .sum();

    // Create an empty storage
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit,
        ..Default::default()
    });

    // Insert them all to the storage
    let mut maybe_inserted_transactions = Vec::new();
    let mut some_rejected_transactions = Vec::new();
    for unmined_transaction in unmined_transactions.clone() {
        let result = storage.insert(unmined_transaction.clone(), Vec::new(), None);
        match result {
            Ok(_) => {
                // While the transaction was inserted here, it can be rejected later.
                maybe_inserted_transactions.push(unmined_transaction);
            }
            Err(_) => {
                // Other transactions can be rejected on a successful insert,
                // so not all rejected transactions will be added.
                // Note that `some_rejected_transactions` can be empty since `insert` only
                // returns a rejection error if the transaction being inserted is the one
                // that was randomly evicted.
                some_rejected_transactions.push(unmined_transaction);
            }
        }
    }
    // Since transactions are rejected randomly we can't test exact numbers.
    // We know the first MEMPOOL_TX_COUNT must have been inserted successfully.
    assert!(maybe_inserted_transactions.len() >= MEMPOOL_TX_COUNT);
    assert_eq!(
        some_rejected_transactions.len() + maybe_inserted_transactions.len(),
        unmined_transactions.len()
    );

    // Test if the actual number of inserted/rejected transactions is consistent.
    assert!(storage.verified.transaction_count() <= maybe_inserted_transactions.len());
    assert!(storage.rejected_transaction_count() >= some_rejected_transactions.len());

    // Test if rejected transactions were actually rejected.
    for tx in some_rejected_transactions.iter() {
        assert!(!storage.contains_transaction_exact(&tx.transaction.id.mined_id()));
    }

    // Query all the ids we have for rejected, get back `total - MEMPOOL_SIZE`
    let all_ids: HashSet<UnminedTxId> = unmined_transactions
        .iter()
        .map(|tx| tx.transaction.id)
        .collect();

    // Convert response to a `HashSet`, because the order of the response doesn't matter.
    let all_rejected_ids: HashSet<UnminedTxId> = storage.rejected_transactions(all_ids).collect();

    let some_rejected_ids = some_rejected_transactions
        .iter()
        .map(|tx| tx.transaction.id)
        .collect::<HashSet<_>>();

    // Test if the rejected transactions we have are a subset of the actually
    // rejected transactions.
    assert!(some_rejected_ids.is_subset(&all_rejected_ids));

    Ok(())
}

#[test]
fn mempool_storage_crud_same_effects_mainnet() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: 160_000_000,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    // Get one (1) unmined transaction
    let unmined_tx_1 = network
        .unmined_transactions_in_blocks(..)
        .next_back()
        .expect("at least one unmined transaction");

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx_1.clone(), Vec::new(), None);

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains_transaction_exact(&unmined_tx_1.transaction.id.mined_id()));

    // Reject and remove mined tx
    let removal_count = storage
        .reject_and_remove_same_effects(
            &iter::once(unmined_tx_1.transaction.id.mined_id()).collect(),
            vec![unmined_tx_1.transaction.transaction.clone()],
        )
        .total_len();

    // Check that it is /not/ in the mempool as a verified transaction.
    assert_eq!(removal_count, 1);
    assert!(!storage.contains_transaction_exact(&unmined_tx_1.transaction.id.mined_id()));

    // Check that it's rejection is cached in the chain_rejected_same_effects' `Mined` eviction list.
    assert_eq!(
        storage.rejection_error(&unmined_tx_1.transaction.id),
        Some(SameEffectsChainRejectionError::Mined.into())
    );
    assert_eq!(
        storage.insert(unmined_tx_1, Vec::new(), None),
        Err(SameEffectsChainRejectionError::Mined.into())
    );

    // Get a different unmined transaction
    let unmined_tx_2 = network
        .unmined_transactions_in_blocks(1..)
        .find(|tx| {
            tx.transaction
                .transaction
                .spent_outpoints()
                .next()
                .is_some()
        })
        .expect("at least one unmined transaction with at least 1 spent outpoint");

    // Insert unmined tx into the mempool.
    assert_eq!(
        storage.insert(unmined_tx_2.clone(), Vec::new(), None),
        Ok(unmined_tx_2.transaction.id)
    );

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains_transaction_exact(&unmined_tx_2.transaction.id.mined_id()));

    // Reject and remove duplicate spend tx
    let removal_count = storage
        .reject_and_remove_same_effects(
            &HashSet::new(),
            vec![unmined_tx_2.transaction.transaction.clone()],
        )
        .total_len();

    // Check that it is /not/ in the mempool as a verified transaction.
    assert_eq!(removal_count, 1);
    assert!(!storage.contains_transaction_exact(&unmined_tx_2.transaction.id.mined_id()));

    // Check that it's rejection is cached in the chain_rejected_same_effects' `SpendConflict` eviction list.
    assert_eq!(
        storage.rejection_error(&unmined_tx_2.transaction.id),
        Some(SameEffectsChainRejectionError::DuplicateSpend.into())
    );
    assert_eq!(
        storage.insert(unmined_tx_2, Vec::new(), None),
        Err(SameEffectsChainRejectionError::DuplicateSpend.into())
    );
}

#[test]
fn mempool_expired_basic() -> Result<()> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        mempool_expired_basic_for_network(network)?;
    }
    Ok(())
}

fn mempool_expired_basic_for_network(network: Network) -> Result<()> {
    // Create an empty storage
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: 160_000_000,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let block: Block = network.test_block(982681, 925483).unwrap();

    // Get a test transaction
    let tx = &*(block.transactions[1]).clone();

    // Change the expiration height of the test transaction
    let mut tx = tx.clone();
    *tx.expiry_height_mut() = zebra_chain::block::Height(1);

    let tx_id = tx.unmined_id();

    // Insert the transaction into the mempool, with a fake zero miner fee and sigops
    storage.insert(
        VerifiedUnminedTx::new(
            tx.into(),
            Amount::try_from(1_000_000).expect("valid amount"),
            0,
            std::sync::Arc::new(vec![]),
        )
        .expect("verification should pass"),
        Vec::new(),
        None,
    )?;

    assert_eq!(storage.transaction_count(), 1);

    // Get everything available in mempool now
    let everything_in_mempool: HashSet<UnminedTxId> = storage.tx_ids().collect();
    assert_eq!(everything_in_mempool.len(), 1);
    assert!(everything_in_mempool.contains(&tx_id));

    // remove_expired_transactions() will return what was removed
    let expired = storage.remove_expired_transactions(Height(1));
    assert!(expired.contains(&tx_id));
    let everything_in_mempool: HashSet<UnminedTxId> = storage.tx_ids().collect();
    assert_eq!(everything_in_mempool.len(), 0);

    // No transaction will be sent to peers
    let send_to_peers = Mempool::remove_expired_from_peer_list(&everything_in_mempool, &expired);
    assert_eq!(send_to_peers.len(), 0);

    Ok(())
}

/// Check that the transaction dependencies are updated when transactions with spent mempool outputs
/// are inserted into storage, and that the `Storage.remove()` method also removes any transactions
/// that directly or indirectly spend outputs of a removed transaction.
#[test]
fn mempool_removes_dependent_transactions() -> Result<()> {
    let network = Network::Mainnet;

    // Create an empty storage
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: 160_000_000,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let unmined_txs_with_transparent_outputs = || {
        network.unmined_transactions_in_blocks(..).filter(|tx| {
            // treat outputs < 100 zatoshis as "dust" for these tests, we want them out
            let dust_threshold: Amount<NonNegative> =
                Amount::try_from(100u64).expect("valid amount");
            !tx.transaction.transaction.outputs().is_empty()
                && tx
                    .transaction
                    .transaction
                    .outputs()
                    .iter()
                    .all(|out| out.value >= dust_threshold)
        })
    };

    let mut fake_spent_outpoints: Vec<OutPoint> = Vec::new();
    let mut expected_transaction_dependencies = HashMap::new();
    let mut expected_transaction_dependents = HashMap::new();
    for unmined_tx in unmined_txs_with_transparent_outputs() {
        let tx_id = unmined_tx.transaction.id.mined_id();
        let num_outputs = unmined_tx.transaction.transaction.outputs().len();

        if let Some(&fake_spent_outpoint) = fake_spent_outpoints.first() {
            expected_transaction_dependencies
                .insert(tx_id, [fake_spent_outpoint.hash].into_iter().collect());
            expected_transaction_dependents
                .insert(fake_spent_outpoint.hash, [tx_id].into_iter().collect());
        }

        storage
            .insert(unmined_tx.clone(), fake_spent_outpoints, None)
            .expect("should insert transaction");

        // Add up to 5 of this transaction's outputs as fake spent outpoints for the next transaction
        fake_spent_outpoints = (0..num_outputs.min(5))
            .map(|i| OutPoint::from_usize(tx_id, i))
            .collect();
    }

    assert_eq!(
        storage.transaction_dependencies().dependencies().len(),
        unmined_txs_with_transparent_outputs()
            .count()
            .checked_sub(1)
            .expect("at least one unmined transaction with transparent outputs"),
        "should have an entry all inserted txns except the first one"
    );

    assert_eq!(
        storage.transaction_dependencies().dependencies(),
        &expected_transaction_dependencies,
        "should have expected transaction dependencies"
    );

    assert_eq!(
        storage.transaction_dependencies().dependents(),
        &expected_transaction_dependents,
        "should have expected transaction dependents"
    );

    // Remove the first transaction and check that everything in storage is emptied.
    let first_tx = unmined_txs_with_transparent_outputs()
        .next()
        .expect("at least one unmined transaction with transparent outputs");

    let expected_num_removed = storage.transaction_count();
    let num_removed = storage.remove_exact(&[first_tx.transaction.id].into_iter().collect());

    assert_eq!(
        num_removed, expected_num_removed,
        "remove_exact should total storage transaction count"
    );

    assert!(
        storage.transaction_dependencies().dependencies().is_empty(),
        "tx deps should be empty"
    );

    assert_eq!(
        storage.transaction_count(),
        0,
        "verified set should be empty"
    );

    Ok(())
}

// ---- Policy function unit tests ----

/// Build a P2PKH lock script: OP_DUP OP_HASH160 <20-byte hash> OP_EQUALVERIFY OP_CHECKSIG
fn p2pkh_script(hash: &[u8; 20]) -> transparent::Script {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend_from_slice(hash);
    s.push(0x88);
    s.push(0xac);
    transparent::Script::new(&s)
}

/// Build a P2SH lock script: OP_HASH160 <20-byte hash> OP_EQUAL
fn p2sh_lock_script(hash: &[u8; 20]) -> transparent::Script {
    let mut s = vec![0xa9, 0x14];
    s.extend_from_slice(hash);
    s.push(0x87);
    transparent::Script::new(&s)
}

#[test]
fn standard_script_kind_classifies_p2pkh() {
    let _init_guard = zebra_test::init();
    let script = p2pkh_script(&[0xaa; 20]);
    let kind = super::super::standard_script_kind(&script);
    assert!(
        matches!(
            kind,
            Some(zcash_script::solver::ScriptKind::PubKeyHash { .. })
        ),
        "P2PKH script should be classified as PubKeyHash"
    );
}

#[test]
fn standard_script_kind_classifies_p2sh() {
    let _init_guard = zebra_test::init();
    let script = p2sh_lock_script(&[0xbb; 20]);
    let kind = super::super::standard_script_kind(&script);
    assert!(
        matches!(
            kind,
            Some(zcash_script::solver::ScriptKind::ScriptHash { .. })
        ),
        "P2SH script should be classified as ScriptHash"
    );
}

#[test]
fn standard_script_kind_classifies_op_return() {
    let _init_guard = zebra_test::init();
    // OP_RETURN followed by data
    let script = transparent::Script::new(&[0x6a, 0x04, 0xde, 0xad, 0xbe, 0xef]);
    let kind = super::super::standard_script_kind(&script);
    assert!(
        matches!(
            kind,
            Some(zcash_script::solver::ScriptKind::NullData { .. })
        ),
        "OP_RETURN script should be classified as NullData"
    );
}

#[test]
fn standard_script_kind_rejects_nonstandard() {
    let _init_guard = zebra_test::init();
    // Random garbage bytes should not be a standard script
    let script = transparent::Script::new(&[0xff, 0xfe, 0xfd]);
    let kind = super::super::standard_script_kind(&script);
    assert!(
        kind.is_none(),
        "non-standard script should be classified as None"
    );
}

#[test]
fn count_script_push_ops_counts_pushes() {
    let _init_guard = zebra_test::init();
    // Script with 3 push operations: OP_0 <push 1 byte> <push 1 byte>
    let script_bytes = vec![0x00, 0x01, 0xaa, 0x01, 0xbb];
    let count = super::super::count_script_push_ops(&script_bytes);
    assert_eq!(count, 3, "should count 3 push operations");
}

#[test]
fn count_script_push_ops_empty_script() {
    let _init_guard = zebra_test::init();
    let count = super::super::count_script_push_ops(&[]);
    assert_eq!(count, 0, "empty script should have 0 push ops");
}

#[test]
fn extract_p2sh_redeemed_script_extracts_last_push() {
    let _init_guard = zebra_test::init();
    // scriptSig: <sig> <redeemed_script>
    // OP_PUSHDATA with 3 bytes "abc" then OP_PUSHDATA with 2 bytes "de"
    let unlock_script = transparent::Script::new(&[0x03, 0x61, 0x62, 0x63, 0x02, 0x64, 0x65]);
    let redeemed = super::super::extract_p2sh_redeemed_script(&unlock_script);
    assert_eq!(
        redeemed,
        Some(vec![0x64, 0x65]),
        "should extract the last push data"
    );
}

#[test]
fn extract_p2sh_redeemed_script_empty_script() {
    let _init_guard = zebra_test::init();
    let unlock_script = transparent::Script::new(&[]);
    let redeemed = super::super::extract_p2sh_redeemed_script(&unlock_script);
    assert!(redeemed.is_none(), "empty scriptSig should return None");
}

#[test]
fn p2sh_sigop_count_length_mismatch_returns_zero() {
    let _init_guard = zebra_test::init();

    // Deserialize a real transaction to get the Transaction type
    let network = Network::Mainnet;
    let block: Block = network.test_block(982681, 925483).unwrap();
    let tx = block.transactions[1].as_ref();

    // Provide wrong number of spent outputs (empty instead of matching inputs)
    let spent_outputs: Vec<transparent::Output> = vec![];
    let count = super::super::p2sh_sigop_count(tx, &spent_outputs);
    assert_eq!(
        count, 0,
        "p2sh_sigop_count should return 0 on length mismatch instead of panicking"
    );
}

#[test]
fn are_inputs_standard_length_mismatch_returns_false() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block: Block = network.test_block(982681, 925483).unwrap();
    let tx = block.transactions[1].as_ref();

    // Provide wrong number of spent outputs
    let spent_outputs: Vec<transparent::Output> = vec![];
    let result = super::super::are_inputs_standard(tx, &spent_outputs);
    assert!(
        !result,
        "are_inputs_standard should return false on length mismatch instead of panicking"
    );
}

#[test]
fn script_sig_args_expected_values() {
    let _init_guard = zebra_test::init();

    // P2PKH expects 2 args (sig + pubkey)
    let pkh_kind = zcash_script::solver::ScriptKind::PubKeyHash { hash: [0xaa; 20] };
    assert_eq!(super::super::script_sig_args_expected(&pkh_kind), Some(2));

    // P2SH expects 1 arg (the redeemed script)
    let sh_kind = zcash_script::solver::ScriptKind::ScriptHash { hash: [0xbb; 20] };
    assert_eq!(super::super::script_sig_args_expected(&sh_kind), Some(1));

    // NullData expects None (non-standard to spend)
    let nd_kind = zcash_script::solver::ScriptKind::NullData { data: vec![] };
    assert_eq!(super::super::script_sig_args_expected(&nd_kind), None);
}
