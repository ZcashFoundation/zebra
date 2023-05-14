//! Wrappers for peer addresses which hide sensitive user and node operator details in logs and
//! metrics.

use std::{
    fmt,
    net::SocketAddr,
    ops::{Deref, DerefMut},
    str::FromStr,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A thin wrapper for [`SocketAddr`] which hides peer IP addresses in logs and metrics.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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

impl FromStr for PeerSocketAddr {
    type Err = <SocketAddr as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

impl<S> From<S> for PeerSocketAddr
where
    S: Into<SocketAddr>,
{
    fn from(value: S) -> Self {
        Self(value.into())
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
