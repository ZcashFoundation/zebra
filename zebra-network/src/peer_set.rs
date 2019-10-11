//! A peer set whose size is dynamically determined by resource constraints.

// Portions of this submodule were adapted from tower-balance,
// which is (c) 2019 Tower Contributors (MIT licensed).

mod discover;
mod set;
mod unready_service;

pub use discover::PeerDiscover;
pub use set::PeerSet;
