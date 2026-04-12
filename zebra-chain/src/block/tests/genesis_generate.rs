//! Tests for genesis block generation.

use chrono::{TimeZone, Utc};

use crate::{
    amount::Amount,
    block::{
        generate::{
            create_genesis_coinbase_script, create_genesis_block_template, create_p2pk_script,
            GenesisBlockParams,
        },
        Hash, ZCASH_BLOCK_VERSION,
    },
    transparent::Script,
    work::difficulty::CompactDifficulty,
};

#[test]
fn genesis_coinbase_script_basic() {
    let _init_guard = zebra_test::init();

    let script = create_genesis_coinbase_script(
        CompactDifficulty(0x1f07ffff),
        "Test Genesis Block",
    );
    assert!(!script.is_empty());
}

#[test]
fn genesis_coinbase_script_matches_zcashd() {
    let _init_guard = zebra_test::init();

    // The actual Zcash genesis coinbase script (from zcashd chainparams.cpp)
    // This is: 04ffff071f0104455a636173683062396334656566386237636334313765653530303165333530303938346236666561333536383361376361633134316130343363343230363438333564333
    const EXPECTED_GENESIS_COINBASE: [u8; 77] = [
        4, 255, 255, 7, 31, 1, 4, 69, 90, 99, 97, 115, 104, 48, 98, 57, 99, 52, 101, 101, 102, 56,
        98, 55, 99, 99, 52, 49, 55, 101, 101, 53, 48, 48, 49, 101, 51, 53, 48, 48, 57, 56, 52, 98,
        54, 102, 101, 97, 51, 53, 54, 56, 51, 97, 55, 99, 97, 99, 49, 52, 49, 97, 48, 52, 51, 99,
        52, 50, 48, 54, 52, 56, 51, 53, 100, 51, 52,
    ];

    // The actual Zcash mainnet/testnet/regtest genesis timestamp
    let timestamp = "Zcash0b9c4eef8b7cc417ee5001e3500984b6fea35683a7cac141a043c42064835d34";
    // The actual genesis difficulty
    let difficulty = CompactDifficulty(0x1f07ffff);

    let script = create_genesis_coinbase_script(difficulty, timestamp);

    assert_eq!(
        script.as_slice(),
        EXPECTED_GENESIS_COINBASE.as_slice(),
        "Generated coinbase script should match the actual Zcash genesis coinbase"
    );
}

#[test]
fn genesis_block_template() {
    let _init_guard = zebra_test::init();

    let params = GenesisBlockParams {
        timestamp_message: "Test Genesis Block".to_string(),
        output_script: Script::new(&[0x51]), // OP_TRUE for testing
        block_time: Utc.timestamp_opt(1477641360, 0).unwrap(),
        difficulty_bits: CompactDifficulty(0x1f07ffff),
        version: ZCASH_BLOCK_VERSION,
        reward: Amount::zero(),
    };

    let block = create_genesis_block_template(params);

    // Verify block structure
    assert_eq!(block.transactions.len(), 1);
    assert!(block.transactions[0].is_coinbase());
    assert_eq!(block.header.previous_block_hash, Hash([0u8; 32]));
    assert_eq!(block.header.version, ZCASH_BLOCK_VERSION);
}

#[test]
fn p2pk_script() {
    let _init_guard = zebra_test::init();

    // Test with Zcash genesis pubkey
    let pubkey = "04678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5f";
    let script = create_p2pk_script(pubkey).unwrap();
    let raw = script.as_raw_bytes();

    // Should be: <push_65_bytes> <65 bytes pubkey> <OP_CHECKSIG>
    assert_eq!(raw[0], 65); // Push 65 bytes
    assert_eq!(raw[66], 0xac); // OP_CHECKSIG
}
