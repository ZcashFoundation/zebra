//! Newtype wrappers assigning semantic meaning to primitive types.

use std::net::SocketAddr;

/// A protocol version magic number.
pub struct Version(pub u32);

/// Bitfield of features to be enabled for this connection.
// Tower provides utilities for service discovery, so this might go
// away in the future in favor of that.
pub struct Services(pub u64);

/// A nonce used in the networking layer to identify messages.
pub struct Nonce(pub u64);

/// A network address but with some extra flavor.
///
/// When a network address is needed somewhere, this structure is
/// used. Network addresses are not prefixed with a timestamp in the
/// version message.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
// XXX this doesn't quite fit here
pub struct NetworkAddress(pub Services, pub SocketAddr);
