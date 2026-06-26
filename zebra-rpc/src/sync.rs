//! Read-state follower: maintains a non-finalized state and chain-tip channels in a standalone
//! [`ReadStateService`](zebra_state::ReadStateService) by syncing a co-located trusted Zebra
//! node's best chain over its indexer gRPC.

mod init;
mod stream;
mod syncer;

pub use init::init_read_state_with_syncer;
pub use syncer::TrustedChainSync;
