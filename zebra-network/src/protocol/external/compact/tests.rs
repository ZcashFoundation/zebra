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
fn compact_short_id_literal_vectors() {
    // Generated independently with Python hashlib.sha256 and handwritten SipHash-2-4:
    // Hash header || LE64(nonce), take the first two LE64 words, then low six SipHash bytes.
    // The fixed mainnet header includes its complete 1,344-byte Equihash solution.
    let block_bytes = &**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES;
    let block = Block::zcash_deserialize(block_bytes).unwrap();
    let mut compact = CompactBlock::from_block(&block, 0x8877_6655_4433_2211).unwrap();
    let keys = compact.keys();
    assert_eq!(keys, (0x1f23_5750_38c4_9917, 0x555b_c930_792d_bafe));

    // The legacy identity is SHA256d of ZIP-243 test vector 0's v4 transaction.
    let v4_txid = transaction::Hash([
        0x76, 0x8b, 0xd1, 0x68, 0x80, 0xc7, 0x70, 0xbb, 0xf0, 0x15, 0x92, 0x73, 0x87, 0xf9, 0xdc,
        0x7e, 0x2e, 0x8d, 0xfe, 0xcc, 0xef, 0x55, 0xc3, 0x57, 0x4a, 0xb4, 0x04, 0xdf, 0x99, 0x23,
        0xbf, 0x80,
    ]);
    assert_eq!(
        short_id(keys, UnminedTxId::Legacy(v4_txid)),
        [0x9b, 0x7c, 0xe3, 0x31, 0x28, 0x95],
    );

    // The witnessed identity is ZIP-244 test vector 0's txid followed by its auth digest.
    let v5_wtxid = transaction::WtxId {
        id: transaction::Hash([
            0x55, 0x2c, 0x96, 0xbd, 0x33, 0x83, 0x4b, 0xa1, 0xa8, 0xa3, 0xec, 0xd8, 0x0a, 0x2c,
            0x9c, 0xb4, 0x11, 0x87, 0x55, 0x3a, 0x3d, 0xcf, 0xe7, 0x92, 0x83, 0x16, 0xbb, 0x70,
            0x70, 0x4b, 0x85, 0xd0,
        ]),
        auth_digest: transaction::AuthDigest([
            0x12, 0x76, 0x7e, 0x5f, 0x67, 0x85, 0x67, 0x36, 0x0f, 0xb3, 0xa1, 0xcb, 0x9c, 0xf8,
            0x58, 0x61, 0x3f, 0xfe, 0x22, 0x63, 0xb6, 0x53, 0xc6, 0xa3, 0x70, 0xee, 0x1f, 0x68,
            0x20, 0xab, 0xdc, 0x57,
        ]),
    };
    assert_eq!(
        short_id(keys, UnminedTxId::Witnessed(v5_wtxid)),
        [0xdf, 0x54, 0x2d, 0x79, 0x0c, 0xbd],
    );

    let mut header_bytes = compact.header.zcash_serialize_to_vec().unwrap();
    assert_eq!(header_bytes.as_slice(), &block_bytes[..1487]);
    // Changing the final solution byte must change the actual compact key derivation.
    header_bytes[1486] ^= 1;
    compact.header = Arc::new(block::Header::zcash_deserialize(header_bytes.as_slice()).unwrap());
    assert_ne!(compact.keys(), keys);
}

#[test]
fn compact_short_id_identity_vectors() {
    let block = fixture();
    let compact = CompactBlock::from_block(&block, 0x0807_0605_0403_0201).unwrap();
    let keys = compact.keys();

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
        match id {
            UnminedTxId::Legacy(id) => {
                assert_eq!(tx.version(), 4);
                assert_ne!(id, variant.hash());
            }
            UnminedTxId::Witnessed(id) => {
                assert!(matches!(tx.version(), 5 | 6));
                assert_eq!(id.id, variant.hash());
                assert_ne!(id.auth_digest, variant.auth_digest().unwrap());
            }
        }
        assert_ne!(short_id(keys, id), short_id(keys, variant_id));
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
