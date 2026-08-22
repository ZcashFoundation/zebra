//! Peer connections over the version 2 Zcash P2P network protocol.
//!
//! This module implements the connection lifecycle of the version 2
//! protocol: the application handshake on the dedicated handshake stream,
//! and the connection task that maps the internal [`Request`](crate::Request)
//! and [`Response`](crate::Response) protocol onto v2 request and
//! announcement streams.
//!
//! The wire formats and the QUIC transport itself are implemented in
//! [`protocol::v2`](crate::protocol::v2).

pub mod connection;
pub mod connector;
pub mod handshake;
pub mod service;

pub use connector::Connector as V2Connector;
pub use service::{Handshake as V2Handshaker, HandshakeRequest as V2HandshakeRequest};
