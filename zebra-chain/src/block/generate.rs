//! Genesis block generation for custom networks.
//!
//! This module provides functionality to create new genesis blocks,
//! similar to zcashd's `CreateGenesisBlock` function.

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};

use crate::{
    amount::{Amount, NonNegative},
    block::{merkle, Block, Hash, Header, Height, ZCASH_BLOCK_VERSION},
    fmt::HexDebug,
    serialization::ZcashSerialize,
    transaction::{LockTime, Transaction},
    transparent::{self, Script},
    work::{difficulty::CompactDifficulty, equihash::Solution},
};

/// Parameters for generating a genesis block.
#[derive(Clone, Debug)]
pub struct GenesisBlockParams {
    /// The timestamp message to embed in the coinbase transaction.
    /// This is traditionally a news headline or other memorable text.
    pub timestamp_message: String,

    /// The output script for the coinbase transaction.
    /// Typically a P2PK script with an unspendable key.
    pub output_script: Script,

    /// The block timestamp as Unix time (seconds since epoch).
    pub block_time: DateTime<Utc>,

    /// The difficulty target in compact form (nBits).
    pub difficulty_bits: CompactDifficulty,

    /// The block version (typically 4 for Zcash).
    pub version: u32,

    /// The coinbase reward (typically 0 for genesis blocks).
    pub reward: Amount<NonNegative>,
}

impl Default for GenesisBlockParams {
    fn default() -> Self {
        // Default values similar to Zcash mainnet genesis
        Self {
            timestamp_message: String::new(),
            output_script: Script::new(&[]),
            block_time: Utc.timestamp_opt(0, 0).unwrap(),
            difficulty_bits: CompactDifficulty(0x1f07ffff),
            version: ZCASH_BLOCK_VERSION,
            reward: Amount::zero(),
        }
    }
}

/// Creates the coinbase script signature for a genesis block.
///
/// The format follows Bitcoin/Zcash convention:
/// - 4 bytes: difficulty bits (little-endian)
/// - 1 byte: push opcode for the number 4
/// - 1 byte: the number 4
/// - N bytes: the timestamp message
///
/// This matches zcashd's genesis coinbase format:
/// `CScript() << 520617983 << CScriptNum(4) << timestamp`
pub fn create_genesis_coinbase_script(
    difficulty_bits: CompactDifficulty,
    timestamp_message: &str,
) -> Vec<u8> {
    let mut script = Vec::new();

    // Push the difficulty bits (520617983 in mainnet = 0x1f07ffff)
    // This is pushed as a 4-byte little-endian integer with a push opcode
    let bits = difficulty_bits.0;
    script.push(0x04); // OP_PUSHBYTES_4
    script.extend_from_slice(&bits.to_le_bytes());

    // Push the number 4 (using CScriptNum encoding)
    script.push(0x01); // OP_PUSHBYTES_1
    script.push(0x04); // The number 4

    // Push the timestamp message
    let msg_bytes = timestamp_message.as_bytes();
    if msg_bytes.len() < 76 {
        script.push(msg_bytes.len() as u8); // OP_PUSHBYTES_N
    } else if msg_bytes.len() < 256 {
        script.push(0x4c); // OP_PUSHDATA1
        script.push(msg_bytes.len() as u8);
    } else {
        script.push(0x4d); // OP_PUSHDATA2
        script.extend_from_slice(&(msg_bytes.len() as u16).to_le_bytes());
    }
    script.extend_from_slice(msg_bytes);

    script
}

/// Creates a genesis coinbase transaction.
///
/// The genesis coinbase transaction is a version 1 transaction with:
/// - A single coinbase input containing the timestamp message
/// - A single output with the genesis reward (typically 0)
pub fn create_genesis_coinbase_transaction(
    timestamp_message: &str,
    output_script: Script,
    reward: Amount<NonNegative>,
    difficulty_bits: CompactDifficulty,
) -> Transaction {
    // Create the coinbase miner data (stored separately from the BIP34 height encoding).
    // The height encoding is prepended automatically during serialization.
    let coinbase_data = create_genesis_coinbase_script(difficulty_bits, timestamp_message);

    let coinbase_input = transparent::Input::Coinbase {
        height: Height(0),
        data: coinbase_data,
        sequence: 0xffffffff,
    };

    let output = transparent::Output {
        value: reward,
        lock_script: output_script,
    };

    Transaction::V1 {
        inputs: vec![coinbase_input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
    }
}

/// Creates an unmined genesis block header template.
///
/// This creates a block header with all fields set except for
/// the nonce and solution, which must be found through mining.
pub fn create_genesis_header_template(
    merkle_root: merkle::Root,
    time: DateTime<Utc>,
    difficulty_bits: CompactDifficulty,
    version: u32,
) -> Header {
    Header {
        version,
        previous_block_hash: Hash([0u8; 32]), // Genesis has no previous block
        merkle_root,
        commitment_bytes: HexDebug([0u8; 32]), // Before any upgrades that use commitments
        time,
        difficulty_threshold: difficulty_bits,
        nonce: HexDebug([0u8; 32]), // Will be set during mining
        solution: Solution::for_proposal(),
    }
}

/// Creates an unmined genesis block template.
///
/// Returns a block with a placeholder solution that needs to be mined
/// using the Equihash solver.
pub fn create_genesis_block_template(params: GenesisBlockParams) -> Block {
    let coinbase_tx = create_genesis_coinbase_transaction(
        &params.timestamp_message,
        params.output_script,
        params.reward,
        params.difficulty_bits,
    );

    let coinbase_tx = Arc::new(coinbase_tx);

    // Compute merkle root from the coinbase transaction
    let merkle_root: merkle::Root = std::iter::once(&coinbase_tx).collect();

    let header = create_genesis_header_template(
        merkle_root,
        params.block_time,
        params.difficulty_bits,
        params.version,
    );

    Block {
        header: Arc::new(header),
        transactions: vec![coinbase_tx],
    }
}

/// Mines a genesis block by finding a valid Equihash solution.
///
/// This function takes a block template and mines it by trying different
/// nonces until a valid solution is found that meets the difficulty target.
///
/// # Arguments
///
/// * `params` - The genesis block parameters
/// * `cancel_fn` - A function that returns `Err(())` to cancel mining
///
/// # Returns
///
/// Returns the mined block with a valid solution, or an error if mining
/// was cancelled.
///
/// # Feature
///
/// This function requires the `internal-miner` feature to be enabled.
#[cfg(feature = "internal-miner")]
pub fn mine_genesis_block<F>(
    params: GenesisBlockParams,
    cancel_fn: F,
) -> Result<Block, crate::work::equihash::SolverCancelled>
where
    F: FnMut() -> Result<(), crate::work::equihash::SolverCancelled>,
{
    use crate::serialization::AtLeastOne;

    let block = create_genesis_block_template(params);

    // Get a copy of the header for mining
    let header = *block.header;

    // Mine the header using the Equihash solver
    let mined_headers: AtLeastOne<Header> = Solution::solve(header, cancel_fn)?;

    // Use the first valid solution
    let mined_header = mined_headers.first();

    Ok(Block {
        header: Arc::new(*mined_header),
        transactions: block.transactions,
    })
}

/// Creates a standard P2PK output script from a public key.
///
/// This follows the format used in Bitcoin/Zcash genesis blocks:
/// `<pubkey> OP_CHECKSIG`
pub fn create_p2pk_script(pubkey_hex: &str) -> Result<Script, hex::FromHexError> {
    let pubkey_bytes = hex::decode(pubkey_hex)?;
    let mut script = Vec::with_capacity(pubkey_bytes.len() + 2);

    // Push the public key
    if pubkey_bytes.len() < 76 {
        script.push(pubkey_bytes.len() as u8);
    } else {
        script.push(0x4c); // OP_PUSHDATA1
        script.push(pubkey_bytes.len() as u8);
    }
    script.extend_from_slice(&pubkey_bytes);

    // OP_CHECKSIG
    script.push(0xac);

    Ok(Script::new(&script))
}

/// Serializes a block to hex format for storage or display.
pub fn block_to_hex(block: &Block) -> String {
    let bytes = block
        .zcash_serialize_to_vec()
        .expect("block serialization should not fail");
    hex::encode(bytes)
}
