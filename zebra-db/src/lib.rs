//! Finalized-state storage and state snapshots for Zebra.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_db")]
// Remove if possible if MSRV is increased
#![allow(unknown_lints)]
#![allow(clippy::manual_is_multiple_of)]

mod request;
mod response;
pub mod state_snapshot;

#[cfg(feature = "indexer")]
pub use request::Spend;
pub use request::{
    CheckpointVerifiedBlock, CommitAids, FinalizedBlock, HashOrHeight, SemanticallyVerifiedBlock,
    Treestate,
};
pub use response::{HistoryCapabilities, KnownBlock};
pub use state_snapshot::SnapshotBase;
