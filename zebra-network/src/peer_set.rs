//! A peer set whose size is dynamically determined by resource constraints.

mod discover;
mod set;

pub use discover::PeerDiscover;
pub use set::PeerSet;
