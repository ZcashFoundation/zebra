use std::sync::Arc;

use crate::{
    block::Block,
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction,
    transparent::Input,
};
use hex::FromHex;

use zebra_test::prelude::*;

/// Returns a serialized [`Input::Coinbase`] with the given `script_sig` bytes
/// and a zero sequence, for feeding to [`Input::zcash_deserialize`].
fn coinbase_input_bytes(script_sig: &[u8]) -> Vec<u8> {
    assert!(
        script_sig.len() < 253,
        "script_sig length needs longer compact-size encoding than this helper writes",
    );

    let mut bytes = Vec::new();
    // Null prevout hash marks the input as a coinbase.
    bytes.extend_from_slice(&[0u8; 32]);
    // Coinbase prevout index is u32::MAX.
    bytes.extend_from_slice(&0xffff_ffff_u32.to_le_bytes());
    // CompactSize-prefixed script_sig.
    bytes.push(script_sig.len() as u8);
    bytes.extend_from_slice(script_sig);
    // Sequence.
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

/// Consensus rule: coinbase height must use the minimum-length BIP-34 encoding.
///
/// Tests the boundary cases that distinguish the OP_N form from the
/// length-prefixed form, and the boundary cases between length-prefixed forms.
/// Adapted from the pre-zip-213 `parse_coinbase_height_mins` test that ran
/// directly against the now-removed `parse_coinbase_height` helper.
///
/// > A coinbase transaction for a block at block height greater than 0 MUST
/// > have a script that, as its first item, encodes the block height `height`
/// > as follows. For `height` in the range {1 .. 16}, the encoding is a single
/// > byte of value `0x50` + `height`. Otherwise, let `heightBytes` be the
/// > signed little-endian representation of `height`, using the minimum
/// > nonzero number of bytes such that the most significant byte is < `0x80`.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
#[test]
fn coinbase_height_minimal_encoding() {
    let _init_guard = zebra_test::init();

    // Asserts that a coinbase script_sig parses as the given height. A trailing
    // padding byte is appended where needed to satisfy the 2-byte minimum
    // coinbase script length.
    fn assert_height(script_sig: &[u8], expected_height: u32) {
        let bytes = coinbase_input_bytes(script_sig);
        match Input::zcash_deserialize(&bytes[..]) {
            Ok(Input::Coinbase { height, .. }) => assert_eq!(
                height.0, expected_height,
                "wrong height for script_sig {script_sig:?}",
            ),
            other => panic!("expected coinbase with height {expected_height}, got {other:?}"),
        }
    }

    // Asserts that a coinbase script_sig is rejected by the deserializer.
    fn assert_rejected(script_sig: &[u8]) {
        let bytes = coinbase_input_bytes(script_sig);
        let result = Input::zcash_deserialize(&bytes[..]);
        assert!(
            result.is_err(),
            "expected error for non-minimal script_sig {script_sig:?}, got {result:?}",
        );
    }

    // Height 1: the only valid encoding is the OP_1 opcode (0x51). Padded with
    // a trailing byte to satisfy the 2-byte minimum coinbase script length.
    assert_height(&[0x51, 0x00], 1);
    // Non-minimal length-prefixed encodings of height 1 must be rejected.
    assert_rejected(&[0x01, 0x01]);
    assert_rejected(&[0x02, 0x01, 0x00]);
    assert_rejected(&[0x03, 0x01, 0x00, 0x00]);
    assert_rejected(&[0x04, 0x01, 0x00, 0x00, 0x00]);

    // Height 17: the only valid encoding is `0x01 0x11` (push 1 byte = 17).
    assert_height(&[0x01, 0x11], 17);
    // Non-minimal length-prefixed encodings of height 17 must be rejected.
    assert_rejected(&[0x02, 0x11, 0x00]);
    assert_rejected(&[0x03, 0x11, 0x00, 0x00]);
    assert_rejected(&[0x04, 0x11, 0x00, 0x00, 0x00]);
}

#[test]
fn get_transparent_output_address() -> Result<()> {
    let _init_guard = zebra_test::init();

    let script_tx: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000")
    .expect("Block bytes are in valid hex representation");

    let transaction = script_tx.zcash_deserialize_into::<Arc<transaction::Transaction>>()?;

    // Hashes were extracted from the transaction (parsed with zebra-chain,
    // then manually extracted from lock_script).
    // Final expected values were generated with https://secretscan.org/PrivateKeyHex,
    // by filling field 4 with the prefix followed by the address hash.
    // Refer to <https://zips.z.cash/protocol/protocol.pdf#transparentaddrencoding>
    // for the prefixes.

    // Script hash 1b8a9bda4b62cd0d0582b55455d0778c86f8628f
    let addr = transaction.outputs()[0]
        .address(&Network::Mainnet)
        .expect("should return address");
    assert_eq!(addr.to_string(), "t3M5FDmPfWNRG3HRLddbicsuSCvKuk9hxzZ");
    let addr = transaction.outputs()[0]
        .address(&Network::new_default_testnet())
        .expect("should return address");
    assert_eq!(addr.to_string(), "t294SGSVoNq2daz15ZNbmAW65KQZ5e3nN5G");
    // Public key hash e4ff5512ffafe9287992a1cd177ca6e408e03003
    let addr = transaction.outputs()[1]
        .address(&Network::Mainnet)
        .expect("should return address");
    assert_eq!(addr.to_string(), "t1ekRwsd4LaSsd6NXgsx66q2HxQWTLCF44y");
    let addr = transaction.outputs()[1]
        .address(&Network::new_default_testnet())
        .expect("should return address");
    assert_eq!(addr.to_string(), "tmWbBGi7TjExNmLZyMcFpxVh3ZPbGrpbX3H");

    Ok(())
}

#[test]
fn get_transparent_output_address_with_blocks() {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        get_transparent_output_address_with_blocks_for_network(network);
    }
}

/// Test that the block test vector indexes match the heights in the block data,
/// and that each post-sapling block has a corresponding final sapling root.
fn get_transparent_output_address_with_blocks_for_network(network: Network) {
    let block_iter = network.block_iter();

    let mut valid_addresses = 0;

    for (&height, block_bytes) in block_iter.skip(1) {
        let block = block_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        for (idx, tx) in block.transactions.iter().enumerate() {
            for output in tx.outputs() {
                let addr = output.address(&network);
                if addr.is_none() && idx == 0 && output.lock_script.as_raw_bytes()[0] == 0x21 {
                    // There are a bunch of coinbase transactions with pay-to-pubkey scripts
                    // which we don't support; skip them
                    continue;
                }
                assert!(
                    addr.is_some(),
                    "address of {output:?}; block #{height}; tx #{idx}; must not be None",
                );
                valid_addresses += 1;
            }
        }
    }
    // Make sure we didn't accidentally skip all vectors
    assert!(valid_addresses > 0);
}
