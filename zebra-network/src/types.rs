//! Newtype wrappers assigning semantic meaning to primitive types.

/// A magic number identifying the network.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Magic(pub [u8; 4]);

/// A protocol version number.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Version(pub u32);

/// Bitfield of features to be enabled for this connection.
// Tower provides utilities for service discovery, so this might go
// away in the future in favor of that.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Services(pub u64);

/// A nonce used in the networking layer to identify messages.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Nonce(pub u64);