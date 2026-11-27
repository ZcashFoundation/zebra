//! Fixed test vectors for mempool storage.

#![allow(clippy::unwrap_in_result)]

use std::iter;

use color_eyre::eyre::Result;

use transparent::OutPoint;
use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Block,
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
fn mempool_storage_transactions_exact_matches_witnessed_id() {
    let _init_guard = zebra_test::init();
    let mut storage = Storage::new(&config::Config::default());
    let unmined_tx = Network::Mainnet
        .unmined_transactions_in_blocks(..)
        .rev()
        .find(|tx| matches!(tx.transaction.id, UnminedTxId::Witnessed(_)))
        .expect("block test vectors contain a witnessed transaction");
    let exact_id = unmined_tx.transaction.id;
    let mut mismatched_id = exact_id;
    mismatched_id
        .auth_digest_mut()
        .expect("witnessed transactions have an authorization digest")
        .0[0] ^= 1;

    assert_eq!(
        storage.insert(unmined_tx.clone(), Vec::new(), None),
        Ok(exact_id)
    );
    assert_eq!(
        storage
            .transactions_exact([exact_id].into())
            .collect::<Vec<_>>(),
        vec![&unmined_tx.transaction]
    );
    assert!(storage
        .transactions_exact([mismatched_id].into())
        .next()
        .is_none());
    assert_eq!(
        storage
            .transactions_exact([exact_id, mismatched_id].into())
            .collect::<Vec<_>>(),
        vec![&unmined_tx.transaction]
    );
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
    tx.set_expiry_height(zebra_chain::block::Height(1));

    let tx_id = tx.unmined_id();

    // Insert the transaction into the mempool, with a fake zero miner fee and sigops
    storage.insert(
        VerifiedUnminedTx::new(
            std::sync::Arc::new(tx).into(),
            Amount::try_from(1_000_000).expect("valid amount"),
            0,
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

    // Each transaction spends outputs of the one before it, so the chain can only be as long as
    // the mempool's limit on unconfirmed ancestors allows.
    let unmined_txs_with_transparent_outputs = || {
        network
            .unmined_transactions_in_blocks(..)
            .filter(|tx| {
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
            .take(MAX_MEMPOOL_ANCESTORS + 1)
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

/// Returns the coinbase transactions of the first Mainnet test blocks, with their outputs replaced
/// by `output_count` standard transparent outputs.
///
/// Tests insert these transactions with outputs of earlier transactions as their spent mempool
/// outputs. The transactions don't spend any outputs or reveal any nullifiers themselves, so they
/// never conflict with each other.
fn transactions_with_outputs(output_count: usize) -> Vec<VerifiedUnminedTx> {
    let output = transparent::Output {
        value: Amount::try_from(10_000).expect("valid amount"),
        lock_script: p2pkh_lock_script(&[0; 20]),
    };

    Network::Mainnet
        .unmined_transactions_in_blocks(1..=10)
        .map(|tx| {
            let mut transaction = tx.transaction.transaction;
            Arc::make_mut(&mut transaction).set_outputs(vec![output.clone(); output_count]);

            VerifiedUnminedTx::new(transaction.into(), tx.miner_fee, 0, 0, Arc::new(Vec::new()))
                .expect("the test fee pays for the replaced outputs")
        })
        .collect()
}

/// Returns the outpoint of the output at `index` in `transaction`.
fn output(transaction: &VerifiedUnminedTx, index: usize) -> OutPoint {
    OutPoint::from_usize(transaction.transaction.id.mined_id(), index)
}

/// Check that the mempool accepts a chain of dependent transactions up to its limit on
/// unconfirmed ancestors, and rejects a transaction that would have one more ancestor.
#[test]
fn mempool_rejects_transaction_with_too_many_unconfirmed_ancestors() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let mut transactions = transactions_with_outputs(1).into_iter();

    // Each transaction spends the output of the one before it, so the last one has the most
    // unconfirmed ancestors that the mempool allows.
    let mut spent_outpoints = Vec::new();
    for _ in 0..=MAX_MEMPOOL_ANCESTORS {
        let tx = transactions
            .next()
            .expect("the test blocks have more transactions than the test uses");
        storage.insert(tx.clone(), spent_outpoints, None)?;
        spent_outpoints = vec![output(&tx, 0)];
    }

    let rejected = transactions
        .next()
        .expect("the test blocks have more transactions than the test uses");
    let error = SameEffectsTipRejectionError::TooManyAncestors;
    let dependencies = storage.transaction_dependencies().clone();

    assert_eq!(
        storage.insert(rejected.clone(), spent_outpoints, None),
        Err(error.clone().into())
    );
    assert_eq!(
        storage.rejection_error(&rejected.transaction.id),
        Some(error.into())
    );
    assert_eq!(storage.transaction_count(), MAX_MEMPOOL_ANCESTORS + 1);
    assert_eq!(
        storage.transaction_dependencies().dependencies(),
        dependencies.dependencies()
    );
    assert_eq!(
        storage.transaction_dependencies().dependents(),
        dependencies.dependents()
    );

    Ok(())
}

/// Check that the mempool rejects a transaction that would make a package of unconfirmed
/// transactions too large, even though it has only one unconfirmed ancestor.
#[test]
fn mempool_rejects_too_many_children_of_unconfirmed_transaction() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    // The parent and these children fill a package, and the parent has an output for one more
    // child.
    let admitted_children = MAX_MEMPOOL_PACKAGE_TRANSACTIONS - 1;
    let mut transactions = transactions_with_outputs(admitted_children + 1).into_iter();

    let parent = transactions
        .next()
        .expect("the test blocks have more transactions than the test uses");
    storage.insert(parent.clone(), Vec::new(), None)?;

    for index in 0..admitted_children {
        let child = transactions
            .next()
            .expect("the test blocks have more transactions than the test uses");
        storage.insert(child, vec![output(&parent, index)], None)?;
    }

    let rejected = transactions
        .next()
        .expect("the test blocks have more transactions than the test uses");
    let error = SameEffectsTipRejectionError::TooManyPackageTransactions;
    let dependencies = storage.transaction_dependencies().clone();

    assert_eq!(
        storage.insert(
            rejected.clone(),
            vec![output(&parent, admitted_children)],
            None
        ),
        Err(error.clone().into())
    );
    assert_eq!(
        storage.rejection_error(&rejected.transaction.id),
        Some(error.into())
    );
    assert_eq!(
        storage.transaction_count(),
        MAX_MEMPOOL_PACKAGE_TRANSACTIONS
    );
    assert_eq!(
        storage.transaction_dependencies().dependencies(),
        dependencies.dependencies()
    );
    assert_eq!(
        storage.transaction_dependencies().dependents(),
        dependencies.dependents()
    );

    Ok(())
}

/// Check that the mempool rejects a transaction that would join two packages of unconfirmed
/// transactions into one that is too large, even though it has an allowed number of unconfirmed
/// ancestors.
#[test]
fn mempool_rejects_transaction_joining_two_unconfirmed_packages() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let mut transactions = transactions_with_outputs(2).into_iter();

    // Two packages, each with a parent and a child that spends the first output of the parent.
    let mut parents = Vec::new();
    for _ in 0..2 {
        let parent = transactions
            .next()
            .expect("the test blocks have more transactions than the test uses");
        let child = transactions
            .next()
            .expect("the test blocks have more transactions than the test uses");

        storage.insert(parent.clone(), Vec::new(), None)?;
        storage.insert(child, vec![output(&parent, 0)], None)?;
        parents.push(parent);
    }

    // This transaction spends the second output of both parents.
    let rejected = transactions
        .next()
        .expect("the test blocks have more transactions than the test uses");
    let error = SameEffectsTipRejectionError::TooManyPackageTransactions;
    let dependencies = storage.transaction_dependencies().clone();

    assert_eq!(
        storage.insert(
            rejected.clone(),
            parents.iter().map(|parent| output(parent, 1)).collect(),
            None
        ),
        Err(error.clone().into())
    );
    assert_eq!(
        storage.rejection_error(&rejected.transaction.id),
        Some(error.into())
    );
    assert_eq!(storage.transaction_count(), 2 * parents.len());
    assert_eq!(
        storage.transaction_dependencies().dependencies(),
        dependencies.dependencies()
    );
    assert_eq!(
        storage.transaction_dependencies().dependents(),
        dependencies.dependents()
    );

    Ok(())
}

/// Check that the mempool accepts a transaction that joins unconfirmed transactions into a
/// package with the maximum number of transactions.
#[test]
fn mempool_accepts_transaction_joining_unconfirmed_parents() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let mut transactions = transactions_with_outputs(1).into_iter();

    // Parents that aren't connected to any other transactions in the mempool. The transaction
    // that spends their outputs has the most unconfirmed ancestors that the mempool allows.
    let mut parents = Vec::new();
    for _ in 0..MAX_MEMPOOL_ANCESTORS {
        let parent = transactions
            .next()
            .expect("the test blocks have more transactions than the test uses");
        storage.insert(parent.clone(), Vec::new(), None)?;
        parents.push(parent);
    }

    let child = transactions
        .next()
        .expect("the test blocks have more transactions than the test uses");

    assert_eq!(
        storage.insert(
            child.clone(),
            parents.iter().map(|parent| output(parent, 0)).collect(),
            None
        ),
        Ok(child.transaction.id)
    );
    assert_eq!(
        storage.transaction_count(),
        MAX_MEMPOOL_PACKAGE_TRANSACTIONS
    );

    Ok(())
}

/// Check that the mempool accepts a transaction that it rejected for having too many unconfirmed
/// ancestors, after a block mines one of those ancestors and the mempool clears its rejections
/// for the new chain tip.
#[test]
fn mempool_accepts_dependent_transaction_after_its_ancestor_is_mined() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let mut transactions = transactions_with_outputs(1).into_iter();

    let mut chain = Vec::new();
    let mut spent_outpoints = Vec::new();
    for _ in 0..=MAX_MEMPOOL_ANCESTORS {
        let tx = transactions
            .next()
            .expect("the test blocks have more transactions than the test uses");
        storage.insert(tx.clone(), spent_outpoints, None)?;
        spent_outpoints = vec![output(&tx, 0)];
        chain.push(tx);
    }

    let dependent = transactions
        .next()
        .expect("the test blocks have more transactions than the test uses");
    let rejection: Result<UnminedTxId, MempoolError> =
        Err(SameEffectsTipRejectionError::TooManyAncestors.into());

    assert_eq!(
        storage.insert(dependent.clone(), spent_outpoints.clone(), None),
        rejection
    );

    // A block mines the first transaction of the chain, and the mempool updates its storage in
    // the same order as it does for a new chain tip.
    let mined = &chain[0];
    let mined_ids: HashSet<_> = [mined.transaction.id.mined_id()].into();
    storage.clear_mined_dependencies(&mined_ids);
    let removed = storage
        .reject_and_remove_same_effects(&mined_ids, vec![mined.transaction.transaction.clone()]);

    assert_eq!(removed.mined, HashSet::from([mined.transaction.id]));
    assert!(removed.invalidated.is_empty());

    // The rejection lasts until the mempool clears the rejections for the old tip.
    assert_eq!(
        storage.insert(dependent.clone(), spent_outpoints.clone(), None),
        rejection
    );

    storage.clear_tip_rejections();

    assert_eq!(
        storage.insert(dependent.clone(), spent_outpoints, None),
        Ok(dependent.transaction.id)
    );
    assert_eq!(storage.transaction_count(), MAX_MEMPOOL_ANCESTORS + 1);

    Ok(())
}

// ---- Policy function unit tests ----

use super::super::policy::{p2pk_lock_script, p2pkh_lock_script, p2sh_lock_script};

#[test]
fn standard_script_kind_classifies_p2pkh() {
    let _init_guard = zebra_test::init();
    let script = p2pkh_lock_script(&[0xaa; 20]);
    let kind = super::super::policy::standard_script_kind(&script);
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
    let kind = super::super::policy::standard_script_kind(&script);
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
    let kind = super::super::policy::standard_script_kind(&script);
    assert!(
        matches!(
            kind,
            Some(zcash_script::solver::ScriptKind::NullData { .. })
        ),
        "OP_RETURN script should be classified as NullData"
    );
}

#[test]
fn standard_script_kind_classifies_p2pk() {
    let _init_guard = zebra_test::init();
    // Compressed pubkey starts with 0x02 or 0x03
    let mut pubkey = [0x02; 33];
    pubkey[1..].fill(0xaa);
    let script = p2pk_lock_script(&pubkey);
    let kind = super::super::policy::standard_script_kind(&script);
    assert!(
        matches!(kind, Some(zcash_script::solver::ScriptKind::PubKey { .. })),
        "P2PK script should be classified as PubKey"
    );
}

#[test]
fn standard_script_kind_rejects_nonstandard() {
    let _init_guard = zebra_test::init();
    // Random garbage bytes should not be a standard script
    let script = transparent::Script::new(&[0xff, 0xfe, 0xfd]);
    let kind = super::super::policy::standard_script_kind(&script);
    assert!(
        kind.is_none(),
        "non-standard script should be classified as None"
    );
}
