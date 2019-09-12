//! An address-with-metadata type used in Bitcoin networking.

use chrono::{DateTime, Utc};
use std::net::SocketAddr;

use crate::types::Services;

/// An address with metadata on its advertised services and last-seen time.
/// 
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
pub struct MetaAddr {
    /// The peer's address.
    addr: SocketAddr,
    services: Services,
    last_seen: DateTime<Utc>,
}
