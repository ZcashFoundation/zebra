//! An address-with-metadata type used in Bitcoin networking.

use chrono::{DateTime, Utc};
use std::net::SocketAddr;

use crate::types::Services;

/// An address with metadata on its advertised services and last-seen time.
/// 
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
pub struct MetaAddr {
    /// The peer's address.
    pub addr: SocketAddr,
    /// The services advertised by the peer.
    pub services: Services,
    /// When the peer was last seen.
    pub last_seen: DateTime<Utc>,
}
