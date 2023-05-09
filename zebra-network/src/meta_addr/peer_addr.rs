//! Wrappers for peer addresses which hide sensitive user and node operator details in logs and
//! metrics.

use std::{
    fmt,
    net::SocketAddr,
    ops::{Deref, DerefMut},
};

/// A thin wrapper for [`SocketAddr`] which hides peer IP addresses in logs and metrics.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerSocketAddr(SocketAddr);

impl fmt::Debug for PeerSocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for PeerSocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ip_version = if self.is_ipv4() { "v4" } else { "v6" };

        // The port is usually not sensitive, and it's useful for debugging.
        f.pad(&format!("{}redacted:{}", ip_version, self.port()))
    }
}

impl Deref for PeerSocketAddr {
    type Target = SocketAddr;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PeerSocketAddr {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
