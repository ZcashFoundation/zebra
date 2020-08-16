use std::env;
use std::io::ErrorKind;

use proptest::{arbitrary::any, prelude::*, test_runner::Config};

use crate::parameters::Network;
use crate::serialization::{SerializationError, ZcashDeserializeInto, ZcashSerialize};

use super::super::{serialize::MAX_BLOCK_BYTES, *};

proptest! {
    #[test]
    fn blockheaderhash_roundtrip(hash in any::<Hash>()) {
        let bytes = hash.zcash_serialize_to_vec()?;
        let other_hash: Hash = bytes.zcash_deserialize_into()?;

        prop_assert_eq![hash, other_hash];
    }

    #[test]
    fn blockheader_roundtrip(header in any::<Header>()) {
        let bytes = header.zcash_serialize_to_vec()?;
        let other_header = bytes.zcash_deserialize_into()?;

        prop_assert_eq![header, other_header];
    }

    #[test]
    fn root_hash_roundtrip(
        bytes in any::<[u8; 32]>(),
        network in any::<Network>(),
        block_height in any::<Height>()
    ) {
        let root_hash = RootHash::from_bytes(bytes, network, block_height);
        let other_bytes = root_hash.to_bytes();

        prop_assert_eq![bytes, other_bytes];
    }
}

proptest! {
    // The block roundtrip test can be really slow, so we use fewer cases by
    // default. Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(Config::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(16)))]

    #[test]
    fn block_roundtrip(block in any::<Block>(), network in any::<Network>()) {
        let bytes = block.zcash_serialize_to_vec()?;
        let bytes = &mut bytes.as_slice();

        // Check the root hash
        let root_hash = block.root_hash(network);
        if let Some(root_hash) = root_hash {
            let root_hash_bytes = root_hash.to_bytes();
            prop_assert_eq![block.header.root_bytes, root_hash_bytes];
        } else {
            prop_assert_eq![block.coinbase_height(), None];
        }

        // Check the block size limit
        if bytes.len() <= MAX_BLOCK_BYTES as _ {
            // Check deserialization
            let other_block = bytes.zcash_deserialize_into()?;

            prop_assert_eq![block, other_block];
        } else {
            let serialization_err = bytes.zcash_deserialize_into::<Block>()
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
