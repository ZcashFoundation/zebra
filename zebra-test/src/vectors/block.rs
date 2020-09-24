//! Block test vectors

use hex::FromHex;
use lazy_static::lazy_static;

// Mainnet
//
// for i in `seq 0 9`; do
//     zcash-cli getblock $i 0 > main-0-000-00$i.txt
// done
const BLOCK_MAINNET_GENESIS_HEX: &str = include_str!("block-main-0-000-000.txt");
const BLOCK_MAINNET_1_HEX: &str = include_str!("block-main-0-000-001.txt");
const BLOCK_MAINNET_2_HEX: &str = include_str!("block-main-0-000-002.txt");
const BLOCK_MAINNET_3_HEX: &str = include_str!("block-main-0-000-003.txt");
const BLOCK_MAINNET_4_HEX: &str = include_str!("block-main-0-000-004.txt");
const BLOCK_MAINNET_5_HEX: &str = include_str!("block-main-0-000-005.txt");
const BLOCK_MAINNET_6_HEX: &str = include_str!("block-main-0-000-006.txt");
const BLOCK_MAINNET_7_HEX: &str = include_str!("block-main-0-000-007.txt");
const BLOCK_MAINNET_8_HEX: &str = include_str!("block-main-0-000-008.txt");
const BLOCK_MAINNET_9_HEX: &str = include_str!("block-main-0-000-009.txt");
// zcash-cli getblock 10 0 > block-main-0-000-010.txt
const BLOCK_MAINNET_10_HEX: &str = include_str!("block-main-0-000-010.txt");

// zcash-cli getblock 415000 0 > block-main-0-415-000.txt
const BLOCK_MAINNET_415000_HEX: &str = include_str!("block-main-0-415-000.txt");
// zcash-cli getblock 434873 0 > block-main-0-434-873.txt
const BLOCK_MAINNET_434873_HEX: &str = include_str!("block-main-0-434-873.txt");

// Testnet
//
// for i in `seq 0 9`; do
//     zcash-cli -testnet getblock $i 0 > block-test-0-000-00$i.txt
// done
const BLOCK_TESTNET_GENESIS_HEX: &str = include_str!("block-test-0-000-000.txt");
const BLOCK_TESTNET_1_HEX: &str = include_str!("block-test-0-000-001.txt");
const BLOCK_TESTNET_2_HEX: &str = include_str!("block-test-0-000-002.txt");
const BLOCK_TESTNET_3_HEX: &str = include_str!("block-test-0-000-003.txt");
const BLOCK_TESTNET_4_HEX: &str = include_str!("block-test-0-000-004.txt");
const BLOCK_TESTNET_5_HEX: &str = include_str!("block-test-0-000-005.txt");
const BLOCK_TESTNET_6_HEX: &str = include_str!("block-test-0-000-006.txt");
const BLOCK_TESTNET_7_HEX: &str = include_str!("block-test-0-000-007.txt");
const BLOCK_TESTNET_8_HEX: &str = include_str!("block-test-0-000-008.txt");
const BLOCK_TESTNET_9_HEX: &str = include_str!("block-test-0-000-009.txt");
// zcash-cli -testnet getblock 10 0 > block-test-0-000-010.txt
const BLOCK_TESTNET_10_HEX: &str = include_str!("block-test-0-000-010.txt");

lazy_static! {
    pub static ref TEST_BLOCKS: Vec<&'static [u8]> = vec![
        &BLOCK_MAINNET_GENESIS_BYTES,
        &BLOCK_MAINNET_1_BYTES,
        &BLOCK_MAINNET_415000_BYTES,
        &BLOCK_MAINNET_434873_BYTES
    ];
    pub static ref BLOCK_MAINNET_GENESIS_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(BLOCK_MAINNET_GENESIS_HEX.trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_1_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_1_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_2_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_2_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_3_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_3_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_4_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_4_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_5_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_5_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_6_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_6_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_7_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_7_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_8_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_8_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_9_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_MAINNET_9_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(BLOCK_MAINNET_10_HEX.trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_415000_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(BLOCK_MAINNET_415000_HEX.trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_MAINNET_434873_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(BLOCK_MAINNET_434873_HEX.trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_GENESIS_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(BLOCK_TESTNET_GENESIS_HEX.trim())
            .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_1_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_1_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_2_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_2_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_3_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_3_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_4_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_4_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_5_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_5_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_6_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_6_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_7_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_7_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_8_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_8_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_9_BYTES: Vec<u8> = <Vec<u8>>::from_hex(BLOCK_TESTNET_9_HEX.trim())
        .expect("Block bytes are in valid hex representation");
    pub static ref BLOCK_TESTNET_10_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(BLOCK_TESTNET_10_HEX.trim())
            .expect("Block bytes are in valid hex representation");
}
