//! Consumed-size evidence must replace only the block-size serialization pass.

use std::sync::atomic::{AtomicUsize, Ordering};

use zebra_chain::transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction};

use super::*;

#[test]
fn decoded_size_matches_canonical_blocks_and_bundles() {
    let _init_guard = zebra_test::init();
    let mut versions = std::collections::HashSet::new();
    for bytes in zebra_test::vectors::BLOCKS.iter() {
        let decoded = DecodedBlock::deserialize(bytes).unwrap();
        assert_eq!(decoded.consumed_size, Some(bytes.len()));
        assert_eq!(decoded.block.zcash_serialized_size(), bytes.len());
        assert!(decoded.matched.is_none());
        versions.extend(decoded.block.transactions.iter().map(|tx| tx.version()));
    }
    assert!((1..=5).all(|version| versions.contains(&version)));

    let mut block = (*block::genesis::regtest_genesis_block()).clone();
    for upgrade in [NetworkUpgrade::Nu6_3, NetworkUpgrade::Nu7] {
        let branch = upgrade.branch_id().unwrap().try_into().unwrap();
        for orchard in [false, true] {
            for ironwood in [false, true] {
                block.transactions = vec![Arc::new(fake_v6_transaction(
                    upgrade,
                    orchard.then(|| {
                        fake_bundle_for_branch(branch, ::orchard::ValuePool::Orchard, 2, 0).unwrap()
                    }),
                    ironwood.then(|| {
                        fake_bundle_for_branch(branch, ::orchard::ValuePool::Ironwood, 2, 1)
                            .unwrap()
                    }),
                ))];
                let bytes = block.zcash_serialize_to_vec().unwrap();
                let decoded = DecodedBlock::deserialize(&bytes).unwrap();
                assert_eq!(decoded.consumed_size, Some(bytes.len()));
                assert_eq!(decoded.block.zcash_serialized_size(), bytes.len());
                assert_eq!(*decoded.block, block);
            }
        }
    }
}

fn block_with_size(size: usize) -> Arc<Block> {
    let mut block: Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let transaction = &block.transactions[0];
    let mut outputs = transaction.outputs();
    // Keep the script's CompactSize prefix five bytes long while adjusting its payload.
    outputs[0].lock_script = transparent::Script::new(&vec![0; 65_536]);
    block.transactions[0] = Arc::new(
        transaction
            .as_ref()
            .clone()
            .with_transparent_outputs(outputs),
    );
    let script_size = size - block.zcash_serialized_size() + 65_536;
    let transaction = &block.transactions[0];
    let mut outputs = transaction.outputs();
    outputs[0].lock_script = transparent::Script::new(&vec![0; script_size]);
    block.transactions[0] = Arc::new(
        transaction
            .as_ref()
            .clone()
            .with_transparent_outputs(outputs),
    );
    assert_eq!(block.zcash_serialized_size(), size);
    Arc::new(block)
}

#[test]
fn decoded_size_preserves_bounds_and_parse_errors() {
    let _init_guard = zebra_test::init();
    let limit = usize::try_from(block::MAX_BLOCK_BYTES).unwrap();
    for size in [limit - 1, limit, limit + 1] {
        let bytes = block_with_size(size).zcash_serialize_to_vec().unwrap();
        if size > limit {
            assert!(DecodedBlock::deserialize(&bytes).is_err());
            continue;
        }
        let decoded = DecodedBlock::deserialize(&bytes).unwrap();
        assert_eq!(decoded.consumed_size, Some(size));
        assert_eq!(decoded.block.zcash_serialized_size(), size);
        assert!(DecodedBlock::deserialize(&bytes[..size - 1]).is_err());
        let mut trailing = bytes;
        trailing.extend_from_slice(&[0; 16]);
        let with_trailing = DecodedBlock::deserialize(&trailing).unwrap();
        assert_eq!(with_trailing, decoded);
    }

    let bytes = &**zebra_test::vectors::BLOCK_MAINNET_1_BYTES;
    let decoded = DecodedBlock::deserialize(bytes).unwrap();
    assert_eq!(decoded.consumed_size, Some(bytes.len()));
    let header_size = decoded.block.header.zcash_serialized_size();
    assert_eq!(bytes[header_size], 1);
    let mut noncanonical = bytes[..header_size].to_vec();
    noncanonical.extend_from_slice(&[253, 1, 0]);
    noncanonical.extend_from_slice(&bytes[header_size + 1..]);
    assert!(DecodedBlock::deserialize(&noncanonical).is_err());
    for end in [0, 8, header_size, header_size + 1, bytes.len() - 1] {
        assert!(DecodedBlock::deserialize(&bytes[..end]).is_err());
    }
}

#[tokio::test]
async fn decoded_size_skips_only_size_serialization() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        for constructed in [false, true] {
            for changed_body in [false, true] {
                let bytes = &**zebra_test::vectors::BLOCK_MAINNET_1_BYTES;
                let mut source: Block = bytes.zcash_deserialize_into().unwrap();
                if changed_body {
                    let transaction = &source.transactions[0];
                    let mut outputs = transaction.outputs();
                    outputs[0].lock_script = transparent::Script::new(&[0x51]);
                    source.transactions[0] = Arc::new(
                        transaction
                            .as_ref()
                            .clone()
                            .with_transparent_outputs(outputs),
                    );
                }
                let decoded =
                    DecodedBlock::deserialize(&source.zcash_serialize_to_vec().unwrap()).unwrap();
                let decoded = if constructed {
                    DecodedBlock::from(decoded.block())
                } else {
                    decoded
                };
                let calls = Arc::new(AtomicUsize::new(0));
                let transactions = tower::service_fn({
                    let calls = calls.clone();
                    move |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::future::ready(Err::<tx::BlockResponse, BoxError>(
                            TransactionError::BadBalance.into(),
                        ))
                    }
                });
                let state = tower::service_fn(|request| async move {
                    match request {
                        zs::Request::KnownBlock(_) => {
                            Ok::<_, BoxError>(zs::Response::KnownBlock(None))
                        }
                        zs::Request::MiningCandidate(_) => Ok(zs::Response::MiningCandidate(None)),
                        _ => panic!("ordinary decoding must not authorize a contextual commit"),
                    }
                });
                let mut verifier =
                    SemanticBlockVerifier::new(&Network::Mainnet, state, transactions);
                let result = verifier
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::CommitDecoded(decoded, None))
                    .await;
                if changed_body {
                    assert!(matches!(
                        result,
                        Err(VerifyBlockError::Block {
                            source: BlockError::BadMerkleRoot { .. }
                        })
                    ));
                } else {
                    assert!(matches!(
                        result,
                        Err(VerifyBlockError::Transaction(TransactionError::BadBalance))
                    ));
                }
                assert_eq!(calls.load(Ordering::SeqCst), usize::from(!changed_body));
                assert_eq!(
                    verifier.block_size_serializations.load(Ordering::SeqCst),
                    usize::from(constructed)
                );
                assert_eq!(verifier.proposal_key_computations.load(Ordering::SeqCst), 0);

                let oversized =
                    block_with_size(usize::try_from(block::MAX_BLOCK_BYTES).unwrap() + 1);
                let oversized = DecodedBlock::from(oversized);
                assert!(oversized.consumed_size.is_none());
                assert!(matches!(
                    verifier
                        .oneshot(Request::CommitDecoded(oversized, None))
                        .await,
                    Err(VerifyBlockError::OversizedBlock)
                ));
                assert_eq!(calls.load(Ordering::SeqCst), usize::from(!changed_body));
            }
        }
    })
    .await
    .expect("ordinary decoded block checks must not stall");
}
