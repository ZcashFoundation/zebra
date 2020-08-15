//! Definitions of block datastructures.

// block::block is done on purpose and is the most representative name
#![allow(clippy::module_inception)]

mod block;
mod hash;
mod header;
mod height;
mod light_client;
mod serialize;

#[cfg(test)]
mod tests;

pub use block::Block;
pub use hash::BlockHeaderHash;
pub use header::BlockHeader;
pub use height::BlockHeight;
pub use light_client::LightClientRootHash;

/// The error type for Block checks.
// XXX try to remove this -- block checks should be done in zebra-consensus
type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
