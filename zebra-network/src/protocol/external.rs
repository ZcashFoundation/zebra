/// A Tokio codec that transforms an `AsyncRead` into a `Stream` of `Message`s.
pub mod codec;
/// Inventory items.
mod inv;
/// An enum of all supported Bitcoin message types.
mod message;
/// Newtype wrappers for primitive types.
pub mod types;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;

pub use codec::Codec;
pub use inv::InventoryHash;
pub use message::Message;
pub use zebra_chain::serialization::MAX_PROTOCOL_MESSAGE_LEN;
