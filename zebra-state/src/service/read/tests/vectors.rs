//! Fixed test vectors for the ReadStateService.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    orchard,
    parameters::Network::*,
    sapling,
    serialization::ZcashDeserializeInto,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction,
};

use zebra_test::{
    prelude::Result,
    transcript::{ExpectedTranscriptError, Transcript},
};

use crate::{
    init_test_services, populated_state,
    response::MinedTx,
    service::{
        finalized_state::{DiskWriteBatch, ZebraDb},
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
    let (_state, read_state, _latest_chain_tip, _chain_tip_change) = init_test_services(network);

    transcript.check(read_state).await?;

    Ok(())
}

/// Test that ReadStateService responds correctly when the state contains blocks.
#[tokio::test(flavor = "multi_thread")]
async fn populated_read_state_responds_correctly() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks.clone(), Mainnet).await;

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
                }))),
            )];

            let transaction_cases = Transcript::from(transaction_cases);
            transaction_cases.check(read_state.clone()).await?;
        }
    }

    Ok(())
}

/// Tests if Zebra combines the Sapling note commitment subtrees from the finalized and
/// non-finalized states correctly.
#[tokio::test]
async fn test_sapling_subtrees() -> Result<()> {
    let dummy_subtree_root = sapling::tree::Node::default();

    // Prepare the finalized state.
    let db_subtree = NoteCommitmentSubtree::new(0, Height(1), dummy_subtree_root);
    let db = ZebraDb::new(&Config::ephemeral(), Mainnet, true);
    let mut db_batch = DiskWriteBatch::new();
    db_batch.insert_sapling_subtree(&db, &db_subtree);
    db.write(db_batch)
        .expect("Writing a batch with a Sapling subtree should succeed.");

    // Prepare the non-fianlized state.
    let chain_subtree = NoteCommitmentSubtree::new(1, Height(3), dummy_subtree_root);
    let mut chain = Chain::default();
    chain.insert_sapling_subtree(chain_subtree);
    let chain = Some(Arc::new(chain));

    // At this point, we have one Sapling subtree in the finalized state and one Sapling subtree in
    // the non-finalized state.

    // Retrieve only the first subtree and check its properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, 0.into(), Some(1.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));

    // Retrieve both subtrees using a limit and check their properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, 0.into(), Some(2.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve both subtrees without using a limit and check their properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, 0.into(), None);
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree and check its properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, 1.into(), Some(1.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, using a limit that would allow for more trees if they were
    // present, and check its properties.
    let subtrees = sapling_subtrees(chain.clone(), &db, 1.into(), Some(2.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, without using any limit, and check its properties.
    let subtrees = sapling_subtrees(chain, &db, 1.into(), None);
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
    let db = ZebraDb::new(&Config::ephemeral(), Mainnet, true);
    let mut db_batch = DiskWriteBatch::new();
    db_batch.insert_orchard_subtree(&db, &db_subtree);
    db.write(db_batch)
        .expect("Writing a batch with an Orchard subtree should succeed.");

    // Prepare the non-fianlized state.
    let chain_subtree = NoteCommitmentSubtree::new(1, Height(3), dummy_subtree_root);
    let mut chain = Chain::default();
    chain.insert_orchard_subtree(chain_subtree);
    let chain = Some(Arc::new(chain));

    // At this point, we have one Orchard subtree in the finalized state and one Orchard subtree in
    // the non-finalized state.

    // Retrieve only the first subtree and check its properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, 0.into(), Some(1.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));

    // Retrieve both subtrees using a limit and check their properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, 0.into(), Some(2.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve both subtrees without using a limit and check their properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, 0.into(), None);
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 2);
    assert!(subtrees_eq(subtrees.next().unwrap(), &db_subtree));
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree and check its properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, 1.into(), Some(1.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, using a limit that would allow for more trees if they were
    // present, and check its properties.
    let subtrees = orchard_subtrees(chain.clone(), &db, 1.into(), Some(2.into()));
    let mut subtrees = subtrees.iter();
    assert_eq!(subtrees.len(), 1);
    assert!(subtrees_eq(subtrees.next().unwrap(), &chain_subtree));

    // Retrieve only the second subtree, without using any limit, and check its properties.
    let subtrees = orchard_subtrees(chain, &db, 1.into(), None);
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
