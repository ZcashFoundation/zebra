//! Genesis consensus parameters for each Zcash network.

use zebra_chain::{block, parameters::Network};

/// The previous block hash for the genesis block.
///
/// All known networks use the Bitcoin `null` value for the parent of the
/// genesis block. (In Bitcoin, `null` is `[0; 32]`.)
pub const GENESIS_PREVIOUS_BLOCK_HASH: block::Hash = block::Hash([0; 32]);

/// Returns the hash for the genesis block in `network`.
pub fn genesis_hash(network: Network) -> block::Hash {
    match network {
        // zcash-cli getblockhash 0 | zebrad revhex
        Network::Mainnet => "08ce3d9731b000c08338455c8a4a6bd05da16e26b11daa1b917184ece80f0400",
        // zcash-cli -testnet getblockhash 0 | zebrad revhex
        Network::Testnet => "382c4a332661c7ed0671f32a34d724619f086c61873bce7c99859dd9920aa605",
    }
    .parse()
    .expect("hard-coded hash parses")
}
