//! Blocks and block-related structures (heights, headers, etc.)

// block::block is done on purpose and is the most representative name
#![allow(clippy::module_inception)]

mod block;
mod hash;
mod header;
mod height;
mod root_hash;
mod serialize;

pub mod merkle;

#[cfg(test)]
mod tests;

pub use block::Block;
pub use hash::BlockHeaderHash;
pub use header::BlockHeader;
pub use height::BlockHeight;
pub use root_hash::RootHash;

/// The error type for Block checks.
// XXX try to remove this -- block checks should be done in zebra-consensus
type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
