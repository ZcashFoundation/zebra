//! `genesis` subcommand - generates a genesis block for a new network.
//!
//! This command creates a genesis block by:
//! 1. Building a coinbase transaction with a timestamp message
//! 2. Creating a block header with the specified parameters
//! 3. Mining an Equihash solution that meets the difficulty target
//!
//! The resulting block can be used to bootstrap a new Zcash-compatible network.

use abscissa_core::{Command, Runnable};
use chrono::{TimeZone, Utc};
use clap::Parser;

use zebra_chain::{
    amount::Amount,
    block::{
        generate::{
            block_to_hex, create_p2pk_script, mine_genesis_block, GenesisBlockParams,
        },
        ZCASH_BLOCK_VERSION,
    },
    work::difficulty::CompactDifficulty,
};

/// The default public key used in the Zcash genesis block output script.
/// This is the same key used in Bitcoin's genesis block (Satoshi's key).
const DEFAULT_GENESIS_PUBKEY: &str = "04678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5f";

/// Generate a genesis block for a new network.
///
/// This command mines a new genesis block using the Equihash proof-of-work algorithm.
/// The resulting block can be used to bootstrap a custom Zcash-compatible network.
#[derive(Command, Debug, Parser)]
pub struct GenesisCmd {
    /// The timestamp message to embed in the coinbase transaction.
    /// Traditionally a news headline or memorable text.
    #[clap(
        long,
        short = 'm',
        default_value = "Custom Zcash Genesis Block",
        help = "Timestamp message for the coinbase transaction"
    )]
    message: String,

    /// Hex-encoded public key for the coinbase output script (P2PK format).
    /// If not provided, uses the default Bitcoin/Zcash genesis pubkey.
    #[clap(
        long,
        short = 'k',
        help = "Hex-encoded public key for P2PK output script"
    )]
    pubkey: Option<String>,

    /// Block timestamp as Unix time (seconds since epoch).
    /// Defaults to the current time.
    #[clap(long, short = 't', help = "Block timestamp as Unix time")]
    time: Option<i64>,

    /// Difficulty target in compact nBits format (hex).
    /// Default is 0x1f07ffff (Zcash mainnet genesis difficulty).
    #[clap(
        long,
        short = 'd',
        default_value = "1f07ffff",
        help = "Difficulty target in compact nBits format (hex)"
    )]
    difficulty: String,

    /// Block version number.
    /// Defaults to 4 (standard Zcash block version).
    #[clap(long, short = 'v', default_value_t = ZCASH_BLOCK_VERSION)]
    version: u32,

    /// Coinbase reward in zatoshis.
    /// Defaults to 0 (standard for genesis blocks).
    #[clap(long, short = 'r', default_value_t = 0)]
    reward: i64,

    /// Output file for the genesis block hex.
    /// If not specified, prints to stdout.
    #[clap(long, short = 'o', help = "Output file for genesis block hex")]
    output: Option<String>,
}

impl Runnable for GenesisCmd {
    #[allow(clippy::print_stdout, clippy::print_stderr)]
    fn run(&self) {
        eprintln!("Generating genesis block...");
        eprintln!("  Message: {}", self.message);

        // Parse the public key or use default
        let output_script = match &self.pubkey {
            Some(pk) => match create_p2pk_script(pk) {
                Ok(script) => script,
                Err(e) => {
                    eprintln!("Error: Invalid public key hex: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("  Using default genesis pubkey");
                create_p2pk_script(DEFAULT_GENESIS_PUBKEY)
                    .expect("default pubkey should be valid")
            }
        };

        // Parse the timestamp
        let block_time = match self.time {
            Some(ts) => match Utc.timestamp_opt(ts, 0) {
                chrono::LocalResult::Single(dt) => dt,
                _ => {
                    eprintln!("Error: Invalid timestamp");
                    std::process::exit(1);
                }
            },
            None => Utc::now(),
        };
        eprintln!("  Block time: {block_time}");

        // Parse the difficulty from hex string (e.g., "1f07ffff")
        // The difficulty is stored in big-endian display order
        let difficulty_bits: CompactDifficulty = {
            let hex_str = self.difficulty.trim_start_matches("0x");
            let value = match u32::from_str_radix(hex_str, 16) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error: Invalid hex in difficulty: {e}");
                    std::process::exit(1);
                }
            };
            // Convert to big-endian bytes (display order)
            let bytes = value.to_be_bytes();
            match CompactDifficulty::from_bytes_in_display_order(&bytes) {
                Ok(bits) => bits,
                Err(e) => {
                    eprintln!("Error: Invalid difficulty value: {e}");
                    std::process::exit(1);
                }
            }
        };
        eprintln!("  Difficulty: 0x{}", self.difficulty);

        // Parse the reward
        let reward = match Amount::try_from(self.reward) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: Invalid reward amount: {e}");
                std::process::exit(1);
            }
        };

        let params = GenesisBlockParams {
            timestamp_message: self.message.clone(),
            output_script,
            block_time,
            difficulty_bits,
            version: self.version,
            reward,
        };

        eprintln!("Mining genesis block (this may take a while)...");

        // Mine the genesis block
        let block = match mine_genesis_block(params, || Ok(())) {
            Ok(b) => b,
            Err(_) => {
                eprintln!("Error: Mining was cancelled");
                std::process::exit(1);
            }
        };

        let block_hex = block_to_hex(&block);
        let block_hash = block.hash();

        eprintln!("\nGenesis block mined successfully!");
        eprintln!("  Block hash: {block_hash}");
        eprintln!("  Block size: {} bytes", block_hex.len() / 2);

        // Output the block
        match &self.output {
            Some(output_file) => {
                use std::{fs::File, io::Write};
                match File::create(output_file) {
                    Ok(mut f) => {
                        if let Err(e) = f.write_all(block_hex.as_bytes()) {
                            eprintln!("Error writing to file: {e}");
                            std::process::exit(1);
                        }
                        eprintln!("Genesis block written to: {output_file}");
                    }
                    Err(e) => {
                        eprintln!("Error creating output file: {e}");
                        std::process::exit(1);
                    }
                }
            }
            None => {
                println!("{block_hex}");
            }
        }
    }
}
