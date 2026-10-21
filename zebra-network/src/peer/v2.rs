//! Peer connections over the version 2 protocol.
//!
//! The wire formats live in [`crate::protocol::v2`] and the QUIC transport in
//! [`crate::quic`]; this module runs the protocol over a transport
//! connection, the handshake first and then the connection itself, with
//! shared state held by actor tasks reached over channels (see
//! [`nonces::NonceRegistry`]), never by locks.

pub mod connection;
pub mod handshake;
pub mod nonces;

#[cfg(test)]
mod tests;
