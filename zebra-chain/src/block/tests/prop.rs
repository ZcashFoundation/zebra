use std::env;
use std::io::ErrorKind;

use proptest::{arbitrary::any, prelude::*, test_runner::Config};
use zebra_test::prelude::*;

use crate::serialization::{SerializationError, ZcashDeserializeInto, ZcashSerialize};
use crate::{block, parameters::Network, LedgerState};

use super::super::{serialize::MAX_BLOCK_BYTES, *};

proptest! {
    #[test]
    fn block_hash_roundtrip(hash in any::<Hash>()) {
        zebra_test::init();

        let bytes = hash.zcash_serialize_to_vec()?;
        let other_hash: Hash = bytes.zcash_deserialize_into()?;

        prop_assert_eq![hash, other_hash];
    }

    #[test]
    fn block_hash_display_fromstr_roundtrip(hash in any::<Hash>()) {
        zebra_test::init();

        let display = format!("{}", hash);
        let parsed = display.parse::<Hash>().expect("hash should parse");
        prop_assert_eq!(hash, parsed);
    }

    #[test]
    fn blockheader_roundtrip(header in any::<Header>()) {
        zebra_test::init();

        let bytes = header.zcash_serialize_to_vec()?;
        let other_header = bytes.zcash_deserialize_into()?;

        prop_assert_eq![header, other_header];
    }

    #[test]
    fn commitment_roundtrip(
        bytes in any::<[u8; 32]>(),
        network in any::<Network>(),
        block_height in any::<Height>()
    ) {
        zebra_test::init();

        // just skip the test if the bytes don't parse, because there's nothing
        // to compare with
        if let Ok(commitment) = Commitment::from_bytes(bytes, network, block_height) {
            let other_bytes = commitment.to_bytes();

            prop_assert_eq![bytes, other_bytes];
        }
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
        zebra_test::init();

        let bytes = block.zcash_serialize_to_vec()?;
        let bytes = &mut bytes.as_slice();

        // Check the block commitment
        let commitment = block.commitment(network);
        if let Ok(commitment) = commitment {
            let commitment_bytes = commitment.to_bytes();
            prop_assert_eq![block.header.commitment_bytes, commitment_bytes];
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

#[test]
fn blocks_have_coinbase() -> Result<()> {
    zebra_test::init();

    let strategy = any::<block::Height>()
        .prop_map(|tip_height| LedgerState {
            tip_height,
            is_coinbase: true,
            network: Network::Mainnet,
        })
        .prop_flat_map(Block::arbitrary_with);

    proptest!(|(block in strategy)| {
        let has_coinbase = block.coinbase_height().is_some();
        prop_assert!(has_coinbase);
    });

    Ok(())
}
