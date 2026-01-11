use std::sync::Arc;

use crate::{block::Block, parameters::Network, serialization::ZcashDeserializeInto, transaction};
use hex::FromHex;

use zebra_test::prelude::*;

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
