//! Network protocol types and serialization for the Zcash wire format.

/// Node address wire formats.
mod addr;
/// A Tokio codec that transforms an `AsyncRead` into a `Stream` of `Message`s.
pub mod codec;
/// Inventory items.
mod inv;
/// An enum of all supported Bitcoin message types.
mod message;
/// Newtype wrappers for primitive types.
pub mod types;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;
#[cfg(test)]
mod tests;

pub use addr::{canonical_peer_addr, canonical_socket_addr, AddrInVersion};
pub use codec::Codec;
pub use inv::{InventoryHash, MAX_TX_INV_IN_SENT_MESSAGE};
pub use message::{Message, VersionMessage};
pub use types::{Nonce, Version};

pub use zebra_chain::serialization::MAX_PROTOCOL_MESSAGE_LEN;
