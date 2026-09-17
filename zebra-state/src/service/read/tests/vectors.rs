//! Fixed test vectors for the ReadStateService.

use std::sync::Arc;

use tower::ServiceExt;
use zebra_chain::{
    block::{Block, Hash, Height, MAX_BLOCK_LOCATOR_LENGTH},
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

/// Test that the ReadStateService rejects a `FindForkPoint` locator longer than
/// `MAX_BLOCK_LOCATOR_LENGTH`, rather than performing an unbounded number of lookups.
#[tokio::test]
async fn find_fork_point_rejects_over_long_locator() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Mainnet;
    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        init_test_services(&network).await;

    // One hash over the cap. The contents are irrelevant: the length is checked
    // before any block is looked up.
    let over_long = vec![Hash([0; 32]); MAX_BLOCK_LOCATOR_LENGTH as usize + 1];

    let transcript = Transcript::from(vec![(
        ReadRequest::FindForkPoint {
            known_blocks: over_long,
        },
        Err(ExpectedTranscriptError::Any),
    )]);

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
    let tip_hash = blocks
        .last()
        .expect("populated state has at least one block")
        .hash();

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

        let fork_point_cases = vec![(
            ReadRequest::FindForkPoint {
                known_blocks: vec![block.hash()],
            },
            Ok(ReadResponse::ForkPoint(Some((
                block.coinbase_height().unwrap(),
                block.hash(),
            )))),
        )];
        let fork_point_cases = Transcript::from(fork_point_cases);
        fork_point_cases.check(read_state.clone()).await?;

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
                    best_chain_tip_hash: tip_hash,
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
        (
            ReadRequest::FindForkPoint {
                known_blocks: vec![block.hash()],
            },
            Ok(ReadResponse::ForkPoint(None)),
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
    .expect("opening an ephemeral database should succeed")
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

/// A non-finalized state with two chains forked from the same genesis block.
struct SideChainFixture {
    non_finalized_state: crate::service::non_finalized_state::NonFinalizedState,
    finalized_state: crate::service::finalized_state::FinalizedState,
    best_hash: Hash,
    side_hash: Hash,
}

/// Returns a [`SideChainFixture`] whose genesis block is extended by a best chain block
/// and by a lower-work side chain block.
fn side_chain_fixture() -> Result<SideChainFixture> {
    use crate::{
        arbitrary::Prepare,
        service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
        tests::FakeChainHelper,
    };
    use zebra_chain::{amount::NonNegative, value_balance::ValueBalance};

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
    assert_ne!(
        best_hash, side_hash,
        "unable to create different block hashes"
    );

    // Create state with a finalized and non-finalized component
    let mut non_finalized_state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    // Commit genesis as the first chain
    non_finalized_state.commit_new_chain(genesis.prepare(), &finalized_state)?;

    // Commit best chain block (higher work) - extends the genesis chain
    non_finalized_state.commit_block(best_chain_block.prepare(), &finalized_state)?;

    // Commit side chain block (lower work) - also tries to extend genesis, creating a fork
    non_finalized_state.commit_block(side_chain_block.prepare(), &finalized_state)?;

    // Verify we have 2 chains (genesis extended by best_chain_block, and genesis extended by side_chain_block)
    assert_eq!(
        non_finalized_state.chain_count(),
        2,
        "Should have 2 competing chains"
    );

    Ok(SideChainFixture {
        non_finalized_state,
        finalized_state,
        best_hash,
        side_hash,
    })
}

/// Test that AnyChainBlock finds blocks in side chains, while Block does not.
#[tokio::test(flavor = "multi_thread")]
async fn any_chain_block_finds_side_chain_blocks() -> Result<()> {
    use crate::service::read::block::{any_block, block};

    let _init_guard = zebra_test::init();

    let SideChainFixture {
        non_finalized_state,
        finalized_state,
        best_hash,
        side_hash,
    } = side_chain_fixture()?;

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

/// Test that the any-chain treestate lookups return the tree of the chain containing the
/// requested block, including side chains, while the best-chain-only lookups do not find
/// side chain trees.
#[tokio::test(flavor = "multi_thread")]
async fn any_chain_treestate_finds_side_chain_trees() -> Result<()> {
    use hex::FromHex;
    use zebra_chain::sapling;

    use crate::service::read::tree::{
        any_ironwood_tree, any_orchard_tree, any_sapling_tree, ironwood_tree, orchard_tree,
        sapling_tree,
    };

    let _init_guard = zebra_test::init();

    let SideChainFixture {
        non_finalized_state,
        finalized_state,
        best_hash,
        side_hash,
    } = side_chain_fixture()?;
    let db = &finalized_state.db;

    // The fake blocks have no shielded data, so both chains have the same empty trees.
    // Give each chain distinct trees at its tip, so a lookup that returns the wrong
    // chain's tree fails the root comparisons below.
    let chain_with_distinct_trees = |hash: Hash, seed: u64| -> Arc<Chain> {
        let mut chain = non_finalized_state
            .find_chain(|chain| chain.contains_block_hash(hash))
            .expect("fixture chains contain their tip blocks")
            .as_ref()
            .clone();
        let height = chain
            .height_by_hash(hash)
            .expect("the chain was found by this hash");

        let mut sapling_tree = sapling::tree::NoteCommitmentTree::default();
        let cm_u = <[u8; 32]>::from_hex(
            "225747f3b5d5dab4e5a424f81f85c904ff43286e0f3fd07ef0b8c6a627b11458",
        )
        .expect("the test vector is valid hex");
        let cm_u = sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&cm_u)
            .expect("the test vector is a valid Sapling note commitment");
        for _ in 0..seed {
            sapling_tree.append(cm_u).expect("the tree is not full");
        }

        let mut orchard_tree = orchard::tree::NoteCommitmentTree::default();
        orchard_tree
            .append(seed.into())
            .expect("the tree is not full");

        let mut ironwood_tree = orchard::tree::NoteCommitmentTree::default();
        ironwood_tree
            .append((seed + 100).into())
            .expect("the tree is not full");

        chain
            .sapling_trees_by_height
            .insert(height, Arc::new(sapling_tree));
        chain
            .orchard_trees_by_height
            .insert(height, Arc::new(orchard_tree));
        chain
            .ironwood_trees_by_height
            .insert(height, Arc::new(ironwood_tree));

        Arc::new(chain)
    };

    // Best chain first, like `NonFinalizedState::chain_iter()`.
    let best_chain = chain_with_distinct_trees(best_hash, 1);
    let side_chain = chain_with_distinct_trees(side_hash, 2);
    let chains = [best_chain.clone(), side_chain.clone()];

    let sapling_root = |chain: &Chain| chain.sapling_note_commitment_tree_for_tip().root();
    let orchard_root = |chain: &Chain| chain.orchard_note_commitment_tree_for_tip().root();
    let ironwood_root = |chain: &Chain| chain.ironwood_note_commitment_tree_for_tip().root();

    assert_ne!(sapling_root(&best_chain), sapling_root(&side_chain));
    assert_ne!(orchard_root(&best_chain), orchard_root(&side_chain));
    assert_ne!(ironwood_root(&best_chain), ironwood_root(&side_chain));

    for (hash, chain) in [(best_hash, &best_chain), (side_hash, &side_chain)] {
        assert_eq!(
            any_sapling_tree(chains.iter(), db, hash).map(|tree| tree.root()),
            Some(sapling_root(chain)),
            "any_sapling_tree should find the treestate of the chain containing the block",
        );
        assert_eq!(
            any_orchard_tree(chains.iter(), db, hash).map(|tree| tree.root()),
            Some(orchard_root(chain)),
            "any_orchard_tree should find the treestate of the chain containing the block",
        );
        assert_eq!(
            any_ironwood_tree(chains.iter(), db, hash).map(|tree| tree.root()),
            Some(ironwood_root(chain)),
            "any_ironwood_tree should find the treestate of the chain containing the block",
        );
    }

    // The best-chain lookups find the best chain trees, but not the side chain trees.
    assert_eq!(
        sapling_tree(Some(&best_chain), db, best_hash.into()).map(|tree| tree.root()),
        Some(sapling_root(&best_chain)),
    );
    assert_eq!(
        orchard_tree(Some(&best_chain), db, best_hash.into()).map(|tree| tree.root()),
        Some(orchard_root(&best_chain)),
    );
    assert_eq!(
        ironwood_tree(Some(&best_chain), db, best_hash.into()).map(|tree| tree.root()),
        Some(ironwood_root(&best_chain)),
    );
    assert!(
        sapling_tree(Some(&best_chain), db, side_hash.into()).is_none(),
        "sapling_tree should NOT find side chain treestate by hash"
    );
    assert!(
        orchard_tree(Some(&best_chain), db, side_hash.into()).is_none(),
        "orchard_tree should NOT find side chain treestate by hash"
    );
    assert!(
        ironwood_tree(Some(&best_chain), db, side_hash.into()).is_none(),
        "ironwood_tree should NOT find side chain treestate by hash"
    );

    // The unmodified non-finalized state resolves the same way.
    assert!(any_sapling_tree(non_finalized_state.chain_iter(), db, side_hash).is_some());
    assert!(any_orchard_tree(non_finalized_state.chain_iter(), db, side_hash).is_some());
    assert!(any_ironwood_tree(non_finalized_state.chain_iter(), db, side_hash).is_some());

    // A block in no chain has no treestate.
    let unknown_hash = Hash([0xff; 32]);
    assert!(any_sapling_tree(chains.iter(), db, unknown_hash).is_none());
    assert!(any_orchard_tree(chains.iter(), db, unknown_hash).is_none());
    assert!(any_ironwood_tree(chains.iter(), db, unknown_hash).is_none());

    Ok(())
}

/// Test that the ReadStateService answers the any-chain treestate requests with the
/// matching response variant and the same trees as the best-chain requests, for blocks
/// in the finalized state.
#[tokio::test(flavor = "multi_thread")]
async fn any_chain_treestate_requests_find_finalized_trees() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks.clone(), &Mainnet).await;

    let call = |request: ReadRequest| {
        let read_state = read_state.clone();
        async move {
            read_state
                .oneshot(request)
                .await
                .expect("read requests should succeed")
        }
    };

    for block in &blocks {
        let hash = block.hash();

        let (ReadResponse::SaplingTree(Some(expected)), ReadResponse::SaplingTree(Some(found))) = (
            call(ReadRequest::SaplingTree(hash.into())).await,
            call(ReadRequest::AnyChainSaplingTree(hash)).await,
        ) else {
            panic!("AnyChainSaplingTree should return a Sapling tree for a committed block");
        };
        assert_eq!(expected.root(), found.root());

        let (ReadResponse::OrchardTree(Some(expected)), ReadResponse::OrchardTree(Some(found))) = (
            call(ReadRequest::OrchardTree(hash.into())).await,
            call(ReadRequest::AnyChainOrchardTree(hash)).await,
        ) else {
            panic!("AnyChainOrchardTree should return an Orchard tree for a committed block");
        };
        assert_eq!(expected.root(), found.root());

        let (ReadResponse::IronwoodTree(Some(expected)), ReadResponse::IronwoodTree(Some(found))) = (
            call(ReadRequest::IronwoodTree(hash.into())).await,
            call(ReadRequest::AnyChainIronwoodTree(hash)).await,
        ) else {
            panic!("AnyChainIronwoodTree should return an Ironwood tree for a committed block");
        };
        assert_eq!(expected.root(), found.root());
    }

    let unknown_hash = Hash([0xff; 32]);
    assert!(matches!(
        call(ReadRequest::AnyChainSaplingTree(unknown_hash)).await,
        ReadResponse::SaplingTree(None)
    ));
    assert!(matches!(
        call(ReadRequest::AnyChainOrchardTree(unknown_hash)).await,
        ReadResponse::OrchardTree(None)
    ));
    assert!(matches!(
        call(ReadRequest::AnyChainIronwoodTree(unknown_hash)).await,
        ReadResponse::IronwoodTree(None)
    ));

    Ok(())
}
