use std::io::{Cursor, ErrorKind, Write};

use chrono::NaiveDateTime;
use proptest::{
    arbitrary::{any, Arbitrary},
    prelude::*,
};

use crate::sha256d_writer::Sha256dWriter;

use super::*;

#[cfg(test)]
impl Arbitrary for BlockHeader {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (
            (4u32..2_147_483_647u32),
            any::<BlockHeaderHash>(),
            any::<MerkleTreeRootHash>(),
            any::<SaplingNoteTreeRootHash>(),
            (0i64..4_294_967_296i64),
            any::<u32>(),
            any::<[u8; 32]>(),
            any::<EquihashSolution>(),
        )
            .prop_map(
                |(
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    final_sapling_root_hash,
                    timestamp,
                    bits,
                    nonce,
                    solution,
                )| BlockHeader {
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    final_sapling_root_hash,
                    time: Utc.timestamp(timestamp, 0),
                    bits,
                    nonce,
                    solution,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[test]
fn blockheaderhash_debug() {
    let preimage = b"foo bar baz";
    let mut sha_writer = Sha256dWriter::default();
    let _ = sha_writer.write_all(preimage);

    let hash = BlockHeaderHash(sha_writer.finish());

    assert_eq!(
        format!("{:?}", hash),
        "BlockHeaderHash(\"bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631\")"
    );
}

#[test]
fn blockheaderhash_from_blockheader() {
    let some_bytes = [0; 32];

    let blockheader = BlockHeader {
        version: 4,
        previous_block_hash: BlockHeaderHash(some_bytes),
        merkle_root_hash: MerkleTreeRootHash(some_bytes),
        final_sapling_root_hash: SaplingNoteTreeRootHash(some_bytes),
        time: DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(61, 0), Utc),
        bits: 0,
        nonce: some_bytes,
        solution: EquihashSolution([0; 1344]),
    };

    let hash = BlockHeaderHash::from(&blockheader);

    assert_eq!(
        format!("{:?}", hash),
        "BlockHeaderHash(\"39c92b8c6b582797830827c78d58674c7205fcb21991887c124d1dbe4b97d6d1\")"
    );

    let mut bytes = Cursor::new(Vec::new());

    blockheader
        .zcash_serialize(&mut bytes)
        .expect("these bytes to serialize from a blockheader without issue");

    bytes.set_position(0);
    let other_header = BlockHeader::zcash_deserialize(&mut bytes)
        .expect("these bytes to deserialize into a blockheader without issue");

    assert_eq!(blockheader, other_header);
}

#[test]
fn deserialize_blockheader() {
    // https://explorer.zcha.in/blocks/415000
    let _header =
        BlockHeader::zcash_deserialize(&zebra_test_vectors::HEADER_MAINNET_415000_BYTES[..])
            .expect("blockheader test vector should deserialize");
}

#[test]
fn deserialize_block() {
    Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
        .expect("block test vector should deserialize");
    Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_1_BYTES[..])
        .expect("block test vector should deserialize");
    // https://explorer.zcha.in/blocks/415000
    Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_415000_BYTES[..])
        .expect("block test vector should deserialize");
    // https://explorer.zcha.in/blocks/434873
    // this one has a bad version field
    Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_434873_BYTES[..])
        .expect("block test vector should deserialize");
}

proptest! {

    #[test]
    fn blockheaderhash_roundtrip(hash in any::<BlockHeaderHash>()) {
        let mut bytes = Cursor::new(Vec::new());
        hash.zcash_serialize(&mut bytes)?;

        bytes.set_position(0);
        let other_hash = BlockHeaderHash::zcash_deserialize(&mut bytes)?;

        prop_assert_eq![hash, other_hash];
    }

    #[test]
    fn blockheader_roundtrip(header in any::<BlockHeader>()) {
        let mut bytes = Cursor::new(Vec::new());
        header.zcash_serialize(&mut bytes)?;

        bytes.set_position(0);
        let other_header = BlockHeader::zcash_deserialize(&mut bytes)?;

        prop_assert_eq![header, other_header];
    }

    #[test]
    fn block_roundtrip(block in any::<Block>()) {
        let mut bytes = Cursor::new(Vec::new());
        block.zcash_serialize(&mut bytes)?;

        // Check the block size limit
        if bytes.position() <= MAX_BLOCK_BYTES {
            bytes.set_position(0);
            let other_block = Block::zcash_deserialize(&mut bytes)?;

            prop_assert_eq![block, other_block];
        } else {
            let serialization_err = Block::zcash_deserialize(&mut bytes)
                .expect_err("blocks larger than the maximum size should fail");
            match serialization_err {
                SerializationError::Io(io_err) => {
                    prop_assert_eq![io_err.kind(), ErrorKind::UnexpectedEof];
                }
                _ => {
                    prop_assert!(false,
                                 "blocks larger than the maximum size should fail with an io::Error");
                }
            }
        }
    }

}
