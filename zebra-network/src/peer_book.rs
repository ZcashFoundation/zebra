//! A single-owner actor for the address book.
//!
//! The peer book actor owns the address book outright (#1976): changes and
//! calls travel on one bounded channel and are applied in order on the
//! actor's dedicated blocking thread, so book access needs no locks at all.
//! Everything else observes the book through three interfaces:
//!
//! - [`ChangeSender`]: sends peer status changes, preserving the sender API
//!   connections and handshakes already use.
//! - [`PeerBookHandle`]: a cloneable [`tower::Service`] for requests that
//!   need an answer from the book (sanitized addresses, cache snapshots,
//!   candidate selection, gossip intake).
//! - [`PeerBookReader`]: a cloneable, lock-free read handle serving the
//!   [`AddressBookPeers`](crate::AddressBookPeers) trait from watch
//!   snapshots, for consumers like the RPC server and health endpoints.

mod actor;
pub(crate) mod buckets;
mod handle;
pub(crate) mod intake;
pub(crate) mod misbehavior;
mod reader;

#[cfg(test)]
mod tests;

pub use handle::{ChangeSender, PeerBookHandle, PeerBookRequest, PeerBookResponse};
pub use misbehavior::BanKey;
pub use reader::PeerBookReader;

pub(crate) use actor::spawn_actor;
