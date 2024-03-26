//! Watches for chain tip height updates to determine the minimum supported peer protocol version.

use std::fmt;

use zebra_chain::{block::Height, chain_tip::ChainTip, parameters::Network};

use crate::protocol::external::types::Version;

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

/// A helper type to monitor the chain tip in order to determine the minimum peer protocol version
/// that is currently supported.
pub struct MinimumPeerVersion<C> {
    network: Network,
    chain_tip: C,
    current_minimum: Version,
    has_changed: bool,
}

impl<C> fmt::Debug for MinimumPeerVersion<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // skip the chain tip to avoid locking issues
        f.debug_struct(std::any::type_name::<MinimumPeerVersion<C>>())
            .field("network", &self.network)
            .field("current_minimum", &self.current_minimum)
            .field("has_changed", &self.has_changed)
            .finish()
    }
}

impl<C> MinimumPeerVersion<C>
where
    C: ChainTip,
{
    /// Create a new [`MinimumPeerVersion`] to track the minimum supported peer protocol version
    /// for the current `chain_tip` on the `network`.
    pub fn new(chain_tip: C, network: &Network) -> Self {
        MinimumPeerVersion {
            network: network.clone(),
            chain_tip,
            current_minimum: Version::min_remote_for_height(network, None),
            has_changed: true,
        }
    }

    /// Check if the minimum supported peer version has changed since the last time this was
    /// called.
    ///
    /// The first call returns the current minimum version, and subsequent calls return [`None`]
    /// until the minimum version changes. When that happens, the next call returns the new minimum
    /// version, and subsequent calls return [`None`] again until the minimum version changes once
    /// more.
    pub fn changed(&mut self) -> Option<Version> {
        self.update();

        if self.has_changed {
            self.has_changed = false;
            Some(self.current_minimum)
        } else {
            None
        }
    }

    /// Retrieve the current minimum supported peer protocol version.
    pub fn current(&mut self) -> Version {
        self.update();
        self.current_minimum
    }

    /// Check the current chain tip height to determine the minimum peer version, and detect if it
    /// has changed.
    fn update(&mut self) {
        let height = self.chain_tip.best_tip_height();
        let new_minimum = Version::min_remote_for_height(&self.network, height);

        if self.current_minimum != new_minimum {
            self.current_minimum = new_minimum;
            self.has_changed = true;
        }
    }

    /// Return the current chain tip height.
    ///
    /// If it is not available return height zero.
    pub fn chain_tip_height(&self) -> Height {
        match self.chain_tip.best_tip_height() {
            Some(height) => height,
            None => Height(0),
        }
    }
}

/// A custom [`Clone`] implementation to ensure that the first call to
/// [`MinimumPeerVersion::changed`] after the clone will always return the current version.
impl<C> Clone for MinimumPeerVersion<C>
where
    C: Clone,
{
    fn clone(&self) -> Self {
        MinimumPeerVersion {
            network: self.network.clone(),
            chain_tip: self.chain_tip.clone(),
            current_minimum: self.current_minimum,
            has_changed: true,
        }
    }
}
