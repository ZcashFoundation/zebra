//! The consensus parameters for each Zcash network.
//!
//! Some consensus parameters change based on network upgrades. Each network
//! upgrade happens at a particular block height. Some parameters have a value
//! (or function) before the upgrade height, at the upgrade height, and after
//! the upgrade height. (For example, the value of the reserved field in the
//! block header during the Heartwood upgrade.)
//!
//! Typically, consensus parameters are accessed via a function that takes a
//! `Network` and `BlockHeight`.

use zebra_chain::block::BlockHeaderHash;
use zebra_chain::{Network, Network::*};

/// The previous block hash for the genesis block.
///
/// All known networks use the Bitcoin `null` value for the parent of the
/// genesis block. (In Bitcoin, `null` is `[0; 32]`.)
pub const GENESIS_PREVIOUS_BLOCK_HASH: BlockHeaderHash = BlockHeaderHash([0; 32]);

/// Returns the hash for the genesis block in `network`.
pub fn genesis_hash(network: Network) -> BlockHeaderHash {
    match network {
        // zcash-cli getblockhash 0 | zebrad revhex
        Mainnet => "08ce3d9731b000c08338455c8a4a6bd05da16e26b11daa1b917184ece80f0400",
        // zcash-cli -testnet getblockhash 0 | zebrad revhex
        Testnet => "382c4a332661c7ed0671f32a34d724619f086c61873bce7c99859dd9920aa605",
    }
    .parse()
    .expect("hard-coded hash parses")
}
