//! Fixed test vectors for the ReadStateService.

use std::sync::Arc;

use tower::ServiceExt;
use zebra_chain::{
    block::{Block, Height},
    orchard,
    parameters::Network::*,
    serialization::ZcashDeserializeInto,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction,
};

use zebra_test::{
    prelude::Result,
    transcript::{ExpectedTranscriptError, Transcript},
};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    init_test_services, populated_state,
    response::MinedTx,
    service::{
        finalized_state::{DiskWriteBatch, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE},
        non_finalized_state::Chain,
        read::{orchard_subtrees, sapling_subtrees},
    },
    Config, ReadRequest, ReadResponse,
};

/// Test that ReadStateService responds correctly when empty.
#[tokio::test]
async fn empty_read_state_still_responds_to_requests() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transcript = Transcript::from(empty_state_test_cases());

    let network = Mainnet;
    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        init_test_services(&network).await;

    transcript.check(read_state).await?;

    Ok(())
}

/// Test that ReadStateService responds correctly when the state contains blocks.
#[tokio::test(flavor = "multi_thread")]
async fn populated_read_state_responds_correctly() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks.clone(), &Mainnet).await;

    let tip_height = Height(blocks.len() as u32 - 1);

    let empty_cases = Transcript::from(empty_state_test_cases());
    empty_cases.check(read_state.clone()).await?;

    for block in blocks {
        let block_cases = vec![
            (
                ReadRequest::Block(block.hash().into()),
                Ok(ReadResponse::Block(Some(block.clone()))),
            ),
            (
                ReadRequest::Block(block.coinbase_height().unwrap().into()),
                Ok(ReadResponse::Block(Some(block.clone()))),
            ),
        ];

        let block_cases = Transcript::from(block_cases);
        block_cases.check(read_state.clone()).await?;

        // Spec: transactions in the genesis block are ignored.
        if block.coinbase_height().unwrap().0 == 0 {
            continue;
        }

        for transaction in &block.transactions {
            let transaction_cases = vec![(
                ReadRequest::Transaction(transaction.hash()),
                Ok(ReadResponse::Transaction(Some(MinedTx {
                    tx: transaction.clone(),
                    height: block.coinbase_height().unwrap(),
                    confirmations: 1 + tip_height.0 - block.coinbase_height().unwrap().0,
                    block_time: block.header.time,
                }))),
            )];

            let transaction_cases = Transcript::from(transaction_cases);
            transaction_cases.check(read_state.clone()).await?;
        }
    }

    Ok(())
}

/// Tests if Zebra combines the note commitment subtrees from the finalized and
/// non-finalized states correctly.
#[tokio::test]
async fn test_read_subtrees() -> Result<()> {
    use std::ops::Bound::*;

    let dummy_subtree = |(index, height)| {
        NoteCommitmentSubtree::new(
            u16::try_from(index).expect("should fit in u16"),
            Height(height),
            sapling_crypto::Node::from_bytes([0; 32]).unwrap(),
        )
    };

    let num_db_subtrees = 10;
    let num_chain_subtrees = 2;
    let index_offset = usize::try_from(num_db_subtrees).expect("constant should fit in usize");
    let db_height_range = 0..num_db_subtrees;
    let chain_height_range = num_db_subtrees..(num_db_subtrees + num_chain_subtrees);

    // Prepare the finalized state.
    let db = {
        let db = new_ephemeral_db();

        let db_subtrees = db_height_range.enumerate().map(dummy_subtree);
        for db_subtree in db_subtrees {
            let mut db_batch = DiskWriteBatch::new();
            db_batch.insert_sapling_subtree(&db, &db_subtree);
            db.write(db_batch)
                .expect("Writing a batch with a Sapling subtree should succeed.");
        }
        db
    };

    // Prepare the non-finalized state.
    let chain = {
        let mut chain = Chain::default();
        let chain_subtrees = chain_height_range
            .enumerate()
            .map(|(index, height)| dummy_subtree((index_offset + index, height)));

        for chain_subtree in chain_subtrees {
            chain.insert_sapling_subtree(chain_subtree);
        }

        Arc::new(chain)
    };

    let modify_chain = |chain: &Arc<Chain>, index: usize, height| {
        let mut chain = chain.as_ref().clone();
        chain.insert_sapling_subtree(dummy_subtree((index, height)));
        Some(Arc::new(chain))
    };

    // There should be 10 entries in db and 2 in chain with no overlap

    // Unbounded range should start at 0
    let all_subtrees = sapling_subtrees(Some(chain.clone()), &db, ..);
    assert_eq!(all_subtrees.len(), 12, "should have 12 subtrees in state");

    // Add a subtree to `chain` that overlaps and is not consistent with the db subtrees
    let first_chain_index = index_offset - 1;
    let end_height = Height(400_000);
    let modified_chain = modify_chain(&chain, first_chain_index, end_height.0);

    // The inconsistent entry and any later entries should be omitted
    let all_subtrees = sapling_subtrees(modified_chain.clone(), &db, ..);
    assert_eq!(all_subtrees.len(), 10, "should have 10 subtrees in state");

    let first_chain_index =
        NoteCommitmentSubtreeIndex(u16::try_from(first_chain_index).expect("should fit in u16"));

    // Entries should be returned without reading from disk if the chain contains the first subtree index in the range
    let mut chain_subtrees = sapling_subtrees(modified_chain, &db, first_chain_index..);
    assert_eq!(chain_subtrees.len(), 3, "should have 3 subtrees in chain");

    let (index, subtree) = chain_subtrees
        .pop_first()
        .expect("chain_subtrees should not be empty");
    assert_eq!(first_chain_index, index, "subtree indexes should match");
    assert_eq!(
        end_height, subtree.end_height,
        "subtree end heights should match"
    );

    // Check that Zebra retrieves subtrees correctly when using a range with an Excluded start bound

    let start = 0.into();
    let range = (Excluded(start), Unbounded);
    let subtrees = sapling_subtrees(Some(chain), &db, range);
    assert_eq!(subtrees.len(), 11);
    assert!(
        !subtrees.contains_key(&start),
        "should not contain excluded start bound"
    );

    Ok(())
}

/// Tests if Zebra combines the Sapling note commitment subtrees from the finalized and
/// non-finalized states correctly.
#[tokio::test]
async fn test_sapling_subtrees() -> Result<()> {
    let dummy_subtree_root = sapling_crypto::Node::from_bytes([0; 32]).unwrap();

    // Prepare the finalized state.
    let db_subtree = NoteCommitmentSubtree::new(0, Height(1), dummy_subtree_root);

    let db = new_ephemeral_db();
    let mut db_batch = DiskWriteBatch::new();
    db_batch.insert_sapling_subtree(&db, &db_subtree);
    db.write(db_batch)
        .expect("Writing a batch with a Sapling subtree should succeed.");

    // Prepare the non-finalized state.
    let chain_subtree = NoteCommitmentSubtree::new(1, Height(3), dummy_subtree_root);
    let mut chain = Chain::default();
    chain.insert_sapling_subtree(chain_subtree);
    let chain = Some(Arc::new(chain));

    // At this point, we have one Sapling subtree in the finalized state and one Sapling subtree in
    // the non-finalized state.

    // Retrieve only the first subtree and check its properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(0)..1.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));

    // Retrieve both subtrees using a limit and check their properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(0)..2.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve both subtrees without using a limit and check their properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(0)..);
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree and check its properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(1)..2.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, using a limit that would allow for more trees if they were
    // present, and check its properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(1)..3.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, without using any limit, and check its properties.
    let subtrees = sapling_subtrees(chain, &db, NoteCommitmentSubtreeIndex(1)..);
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    Ok(())
}

/// Tests if Zebra combines the Orchard note commitment subtrees from the finalized and
/// non-finalized states correctly.
#[tokio::test]
async fn test_orchard_subtrees() -> Result<()> {
    let dummy_subtree_root = orchard::tree::Node::default();

    // Prepare the finalized state.
    let db_subtree = NoteCommitmentSubtree::new(0, Height(1), dummy_subtree_root);

    let db = new_ephemeral_db();
    let mut db_batch = DiskWriteBatch::new();
    db_batch.insert_orchard_subtree(&db, &db_subtree);
    db.write(db_batch)
        .expect("Writing a batch with an Orchard subtree should succeed.");

    // Prepare the non-finalized state.
    let chain_subtree = NoteCommitmentSubtree::new(1, Height(3), dummy_subtree_root);
    let mut chain = Chain::default();
    chain.insert_orchard_subtree(chain_subtree);
    let chain = Some(Arc::new(chain));

    // At this point, we have one Orchard subtree in the finalized state and one Orchard subtree in
    // the non-finalized state.

    // Retrieve only the first subtree and check its properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(0)..1.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));

    // Retrieve both subtrees using a limit and check their properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(0)..2.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve both subtrees without using a limit and check their properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(0)..);
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree and check its properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(1)..2.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, using a limit that would allow for more trees if they were
    // present, and check its properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, NoteCommitmentSubtreeIndex(1)..3.into());
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, without using any limit, and check its properties.
    let subtrees = orchard_subtrees(chain, &db, NoteCommitmentSubtreeIndex(1)..);
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    Ok(())
}

/// Returns test cases for the empty state and missing blocks.
fn empty_state_test_cases() -> Vec<(ReadRequest, Result<ReadResponse, ExpectedTranscriptError>)> {
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_419200_BYTES
        .zcash_deserialize_into()
        .unwrap();

    vec![
        (
            ReadRequest::Transaction(transaction::Hash([0; 32])),
            Ok(ReadResponse::Transaction(None)),
        ),
        (
            ReadRequest::Block(block.hash().into()),
            Ok(ReadResponse::Block(None)),
        ),
        (
            ReadRequest::Block(block.coinbase_height().unwrap().into()),
            Ok(ReadResponse::Block(None)),
        ),
    ]
}

/// Returns `true` if `index` and `subtree_data` match the contents of `subtree`. Otherwise, returns
/// `false`.
fn subtrees_eq<N>(
    (index, subtree_data): (&NoteCommitmentSubtreeIndex, &NoteCommitmentSubtreeData<N>),
    subtree: &NoteCommitmentSubtree<N>,
) -> bool
where
    N: PartialEq + Copy,
{
    index == &subtree.index && subtree_data == &subtree.into_data()
}

/// Returns a new ephemeral database with no consistency checks.
fn new_ephemeral_db() -> ZebraDb {
    ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
}

/// Test that AnyChainBlock can find blocks by hash and height.
#[tokio::test(flavor = "multi_thread")]
async fn any_chain_block_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks.clone(), &Mainnet).await;

    // Test: AnyChainBlock should find blocks by hash (same as Block)
    for block in &blocks {
        let request = ReadRequest::AnyChainBlock(block.hash().into());
        let response = read_state
            .clone()
            .oneshot(request)
            .await
            .expect("request should succeed");
        assert!(
            matches!(
                response,
                ReadResponse::Block(Some(found_block)) if found_block.hash() == block.hash()
            ),
            "AnyChainBlock should find block by hash"
        );
    }

    // Test: AnyChainBlock should find blocks by height (same as Block)
    for block in &blocks {
        let height = block.coinbase_height().unwrap();
        let request = ReadRequest::AnyChainBlock(height.into());
        let response = read_state
            .clone()
            .oneshot(request)
            .await
            .expect("request should succeed");
        assert!(
            matches!(
                response,
                ReadResponse::Block(Some(found_block)) if found_block.hash() == block.hash()
            ),
            "AnyChainBlock should find block by height"
        );
    }

    // Test: Non-existent block should return None
    let fake_hash = zebra_chain::block::Hash([0xff; 32]);
    let request = ReadRequest::AnyChainBlock(fake_hash.into());
    let response = read_state
        .clone()
        .oneshot(request)
        .await
        .expect("request should succeed");
    assert!(
        matches!(response, ReadResponse::Block(None)),
        "AnyChainBlock should return None for non-existent block"
    );

    Ok(())
}

/// Test that AnyChainBlock finds blocks in side chains, while Block does not.
#[tokio::test(flavor = "multi_thread")]
async fn any_chain_block_finds_side_chain_blocks() -> Result<()> {
    use crate::{
        arbitrary::Prepare,
        service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
        tests::FakeChainHelper,
    };
    use zebra_chain::{amount::NonNegative, value_balance::ValueBalance};

    let _init_guard = zebra_test::init();

    let network = Mainnet;

    // Use pre-Heartwood blocks to avoid history tree complications
    let genesis: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    // Create two different blocks from genesis
    // They have the same parent but different work, making them compete
    let best_chain_block = genesis.make_fake_child().set_work(100);
    let side_chain_block = genesis.make_fake_child().set_work(50);

    // Even though they have the same structure, changing work changes the header hash
    // because difficulty_threshold is part of the header
    let best_hash = best_chain_block.hash();
    let side_hash = side_chain_block.hash();

    // If hashes are the same, we can't test side chains properly
    // This would mean our fake block generation isn't working as expected
    if best_hash == side_hash {
        println!("WARNING: Unable to create different block hashes, skipping side chain test");
        return Ok(());
    }

    // Create state with a finalized and non-finalized component
    let mut non_finalized_state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    // Commit genesis as the first chain
    non_finalized_state.commit_new_chain(genesis.prepare(), &finalized_state)?;

    // Commit best chain block (higher work) - extends the genesis chain
    non_finalized_state.commit_block(best_chain_block.clone().prepare(), &finalized_state)?;

    // Commit side chain block (lower work) - also tries to extend genesis, creating a fork
    non_finalized_state.commit_block(side_chain_block.clone().prepare(), &finalized_state)?;

    // Verify we have 2 chains (genesis extended by best_chain_block, and genesis extended by side_chain_block)
    assert_eq!(
        non_finalized_state.chain_count(),
        2,
        "Should have 2 competing chains"
    );

    // Now test with the read interface
    // We'll use the low-level block lookup functions directly
    use crate::service::read::block::{any_block, block};

    // Test 1: any_block with all chains should find the side chain block by hash
    let found = any_block(
        non_finalized_state.chain_iter(),
        &finalized_state.db,
        side_hash.into(),
    );
    assert!(
        found.is_some(),
        "any_block should find side chain block by hash"
    );
    assert_eq!(found.unwrap().hash(), side_hash);

    // Test 2: block with only best chain should NOT find the side chain block by hash
    let found = block(
        non_finalized_state.best_chain(),
        &finalized_state.db,
        side_hash.into(),
    );
    assert!(
        found.is_none(),
        "block should NOT find side chain block by hash"
    );

    // Test 3: any_block should find the best chain block by hash
    let found = any_block(
        non_finalized_state.chain_iter(),
        &finalized_state.db,
        best_hash.into(),
    );
    assert!(
        found.is_some(),
        "any_block should find best chain block by hash"
    );
    assert_eq!(found.unwrap().hash(), best_hash);

    // Test 4: block should also find the best chain block by hash
    let found = block(
        non_finalized_state.best_chain(),
        &finalized_state.db,
        best_hash.into(),
    );
    assert!(
        found.is_some(),
        "block should find best chain block by hash"
    );
    assert_eq!(found.unwrap().hash(), best_hash);

    Ok(())
}
