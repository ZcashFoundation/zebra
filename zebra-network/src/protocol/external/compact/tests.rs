use super::*;
use zebra_chain::{
    parameters::NetworkUpgrade,
    transaction::{self, LockTime},
    transparent,
};

fn inputs(script: &[u8]) -> Vec<transparent::Input> {
    vec![transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: transaction::Hash([7; 32]),
            index: 0,
        },
        unlock_script: transparent::Script::new(script),
        sequence: u32::MAX,
    }]
}

fn fixture() -> Block {
    let mut block =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();
    let outputs = vec![transparent::Output {
        value: 1.try_into().unwrap(),
        lock_script: transparent::Script::new(&[0x51]),
    }];
    let lock = LockTime::min_lock_time_timestamp();
    block.transactions.truncate(1);
    block.transactions.extend([
        Arc::new(Transaction::test_v4(
            inputs(&[1]),
            outputs.clone(),
            lock,
            block::Height(0),
        )),
        Arc::new(Transaction::test_v5(
            NetworkUpgrade::Nu5,
            inputs(&[1]),
            outputs.clone(),
            lock,
            block::Height(0),
        )),
        Arc::new(Transaction::test_v6(
            NetworkUpgrade::Nu6_3,
            inputs(&[1]),
            outputs,
            lock,
            block::Height(0),
        )),
    ]);
    Arc::make_mut(&mut block.header).merkle_root = block.transactions.iter().collect();
    block
}

fn response(block: &Block, indexes: &[usize]) -> BlockTransactions {
    BlockTransactions {
        block_hash: block.hash(),
        transactions: indexes
            .iter()
            .map(|index| block.transactions[*index].clone())
            .collect(),
    }
}

#[test]
fn compact_short_id_identity_vectors() {
    let block = fixture();
    let compact = CompactBlock::from_block(&block, 0x0807_0605_0403_0201).unwrap();
    let mut key_bytes = block.header.zcash_serialize_to_vec().unwrap();
    key_bytes.extend_from_slice(&compact.nonce.to_le_bytes());
    let digest = Sha256::digest(&key_bytes);
    let keys = (
        u64::from_le_bytes(digest[..8].try_into().unwrap()),
        u64::from_le_bytes(digest[8..16].try_into().unwrap()),
    );
    assert_eq!(compact.keys(), keys);
    // The key commits to the Equihash solution at the end of the complete header.
    key_bytes[block.header.zcash_serialized_size() - 1] ^= 1;
    assert_ne!(digest, Sha256::digest(key_bytes));

    // SipHash's published length-32 vector, truncated to low 48 bits in wire order.
    let identity = transaction::Hash(std::array::from_fn(|index| u8::try_from(index).unwrap()));
    assert_eq!(
        short_id(
            (0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908),
            UnminedTxId::Legacy(identity),
        ),
        [0xce, 0x7c, 0xf2, 0x72, 0x2f, 0x51],
    );

    for tx in block.transactions.iter().skip(1) {
        let id = UnminedTxId::from(tx.as_ref());
        let variant = tx.as_ref().clone().with_transparent_inputs(inputs(&[2]));
        let variant_id = UnminedTxId::from(&variant);
        let bytes = match id {
            UnminedTxId::Legacy(id) => {
                assert_eq!(tx.version(), 4);
                assert_ne!(id, variant.hash());
                id.0.to_vec()
            }
            UnminedTxId::Witnessed(id) => {
                assert!(matches!(tx.version(), 5 | 6));
                assert_eq!(id.id, variant.hash());
                assert_ne!(id.auth_digest, variant.auth_digest().unwrap());
                <[u8; 64]>::from(id).to_vec()
            }
        };
        let mut hash = SipHasher24::new_with_keys(keys.0, keys.1);
        hash.write(&bytes);
        assert_eq!(
            short_id(compact.keys(), id).as_slice(),
            &hash.finish().to_le_bytes()[..6]
        );
        assert_ne!(
            short_id(compact.keys(), id),
            short_id(compact.keys(), variant_id)
        );
    }
}

#[test]
fn compact_reconstruction_complete_partial_and_prefilled() {
    let block = fixture();
    let compact = Arc::new(CompactBlock::from_block(&block, 42).unwrap());
    let mut candidates: Vec<UnminedTx> =
        block.transactions.iter().skip(1).map(Into::into).collect();
    candidates.push(candidates[0].clone());
    let reconstruction = compact.clone().reconstruct(candidates).unwrap();
    assert!(reconstruction.missing_indexes().is_empty());
    assert_eq!(
        *reconstruction.finish(response(&block, &[])).unwrap(),
        block
    );

    let reconstruction = compact
        .clone()
        .reconstruct(vec![(&block.transactions[2]).into()])
        .unwrap();
    assert_eq!(reconstruction.missing_indexes(), vec![1, 3]);
    assert_eq!(reconstruction.block_hash(), block.hash());
    assert_eq!(
        *reconstruction.finish(response(&block, &[1, 3])).unwrap(),
        block
    );

    let mut compact = (*compact).clone();
    compact.prefilled.push((2, block.transactions[2].clone()));
    compact.short_ids.remove(1);
    let bytes = compact.zcash_serialize_to_vec().unwrap();
    assert_eq!(
        CompactBlock::zcash_deserialize(bytes.as_slice()).unwrap(),
        compact
    );
    let reconstruction = Arc::new(compact).reconstruct(Vec::new()).unwrap();
    assert_eq!(reconstruction.missing_indexes(), vec![1, 3]);
    assert_eq!(
        *reconstruction.finish(response(&block, &[1, 3])).unwrap(),
        block
    );
}

#[test]
fn compact_ambiguous_matches_never_choose_first() {
    let block = fixture();
    let mut compact = CompactBlock::from_block(&block, 11).unwrap();
    let id = UnminedTxId::from(block.transactions[1].as_ref());
    let other = UnminedTxId::from(block.transactions[2].as_ref());
    let short = short_id(compact.keys(), id);
    assert_eq!(compact.matching_ids([id, id]), HashSet::from([id]));
    // Inject the collision at the matcher boundary, without brute-forcing a 48-bit hash.
    assert!(compact
        .matching_short_ids([(short, id), (short, other), (short, id)].into_iter())
        .is_empty());
    compact.short_ids[1] = short;
    assert!(compact.matching_ids([id, id]).is_empty());
    let reconstruction = Arc::new(compact)
        .reconstruct(vec![(&block.transactions[1]).into()])
        .unwrap();
    assert_eq!(reconstruction.missing_indexes(), vec![1, 2, 3]);
}

#[test]
fn compact_recovery_rejects_wrong_hash_count_order_and_authorization() {
    let block = fixture();
    let compact = Arc::new(CompactBlock::from_block(&block, 12).unwrap());
    let mut wrong_hash = response(&block, &[1, 2, 3]);
    wrong_hash.block_hash = block::Hash([0; 32]);
    for invalid in [
        wrong_hash,
        response(&block, &[1, 2]),
        response(&block, &[2, 1, 3]),
        response(&block, &[1, 2, 3, 3]),
    ] {
        assert!(compact
            .clone()
            .reconstruct(Vec::new())
            .unwrap()
            .finish(invalid)
            .is_err());
    }
    let variant = Arc::new(
        block.transactions[2]
            .as_ref()
            .clone()
            .with_transparent_inputs(inputs(&[2])),
    );
    let reconstruction = compact
        .clone()
        .reconstruct(vec![variant.clone().into()])
        .unwrap();
    assert_eq!(reconstruction.missing_indexes(), vec![1, 2, 3]);
    let mut invalid = response(&block, &[1, 2, 3]);
    invalid.transactions[1] = variant;
    assert!(reconstruction.finish(invalid).is_err());
}

#[test]
fn compact_recovery_rejects_merkle_mismatch_and_duplicate_transactions() {
    let mut block = fixture();
    Arc::make_mut(&mut block.header).merkle_root = block::merkle::Root([0; 32]);
    let compact = Arc::new(CompactBlock::from_block(&block, 13).unwrap());
    assert!(compact
        .reconstruct(Vec::new())
        .unwrap()
        .finish(response(&block, &[1, 2, 3]))
        .is_err());

    block.transactions.push(block.transactions[1].clone());
    Arc::make_mut(&mut block.header).merkle_root = block.transactions.iter().collect();
    let compact = Arc::new(CompactBlock::from_block(&block, 13).unwrap());
    let reconstruction = compact
        .reconstruct(block.transactions.iter().skip(1).map(Into::into).collect())
        .unwrap();
    assert_eq!(reconstruction.missing_indexes(), vec![1, 4]);
    assert!(reconstruction.finish(response(&block, &[1, 4])).is_err());
}

#[test]
fn compact_resource_and_coinbase_bounds() {
    let mut block = fixture();
    let compact = Arc::new(CompactBlock::from_block(&block, 14).unwrap());
    let mut candidate = UnminedTx::from(&block.transactions[1]);
    candidate.size = usize::try_from(MAX_BLOCK_BYTES).unwrap() + 1;
    assert!(compact.clone().reconstruct(vec![candidate]).is_err());
    let mut invalid = (*compact).clone();
    invalid.short_ids.resize(
        usize::try_from(Transaction::max_allocation()).unwrap(),
        [0; 6],
    );
    assert!(invalid.validate().is_err());
    invalid = (*compact).clone();
    invalid.prefilled[0].1 = block.transactions[1].clone();
    assert!(invalid.validate().is_err());
    invalid = (*compact).clone();
    invalid.prefilled.push((0, block.transactions[1].clone()));
    assert!(invalid.validate().is_err());

    let oversized = Arc::new(
        block.transactions[1]
            .as_ref()
            .clone()
            .with_transparent_inputs(inputs(&vec![0; usize::try_from(MAX_BLOCK_BYTES).unwrap()])),
    );
    let mut invalid = response(&block, &[1, 2, 3]);
    invalid.transactions[0] = oversized.clone();
    assert!(compact
        .reconstruct(Vec::new())
        .unwrap()
        .finish(invalid)
        .is_err());
    block.transactions[1] = oversized;
    assert!(CompactBlock::from_block(&block, 14).is_err());
}

#[test]
fn compact_differential_indexes_are_canonical_and_bounded() {
    let request = BlockTransactionsRequest {
        block_hash: block::Hash([1; 32]),
        indexes: vec![0, 1, 255],
    };
    let bytes = request.zcash_serialize_to_vec().unwrap();
    assert_eq!(&bytes[32..], &[3, 0, 0, 0xfd, 0xfd, 0]);
    assert_eq!(
        BlockTransactionsRequest::zcash_deserialize(bytes.as_slice()).unwrap(),
        request
    );
    for indexes in [vec![1, 1], vec![2, 1], vec![usize::MAX]] {
        assert!(BlockTransactionsRequest {
            indexes,
            ..request.clone()
        }
        .zcash_serialize_to_vec()
        .is_err());
    }
    for malformed in [
        vec![1, 0xfd, 0, 0],
        vec![1, 0xff, 255, 255, 255, 255, 255, 255, 255, 255],
        vec![2, 0xfe, 0, 0, 1, 0, 0],
    ] {
        let mut bytes = vec![1; 32];
        bytes.extend(malformed);
        assert!(BlockTransactionsRequest::zcash_deserialize(bytes.as_slice()).is_err());
    }

    let block = fixture();
    let mut prefix = block.header.zcash_serialize_to_vec().unwrap();
    prefix.extend_from_slice(&0u64.to_le_bytes());
    for malformed in [
        vec![0, 0],
        vec![0, 1, 1],
        vec![0xfd, 0, 0],
        vec![0, 1, 0xff, 255, 255, 255, 255, 255, 255, 255, 255],
    ] {
        let mut bytes = prefix.clone();
        bytes.extend(malformed);
        assert!(CompactBlock::zcash_deserialize(bytes.as_slice()).is_err());
    }
}

#[test]
fn compact_recovery_decoding_bounds_transaction_bytes() {
    let block = fixture();
    let large = block.transactions[1]
        .as_ref()
        .clone()
        .with_transparent_inputs(inputs(&vec![0; 1_100_000]));
    let mut bytes = block.hash().zcash_serialize_to_vec().unwrap();
    bytes.push(2);
    large.zcash_serialize(&mut bytes).unwrap();
    large.zcash_serialize(&mut bytes).unwrap();
    assert!(BlockTransactions::zcash_deserialize(bytes.as_slice()).is_err());
    let large = Arc::new(large);
    assert!(BlockTransactions {
        block_hash: block.hash(),
        transactions: vec![large; 2]
    }
    .zcash_serialize_to_vec()
    .is_err());
}
