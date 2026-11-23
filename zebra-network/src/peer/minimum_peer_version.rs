//! Watches for chain tip height updates to determine the minimum supported peer protocol version.

use std::fmt;

use zebra_chain::{chain_tip::ChainTip, parameters::Network};

use crate::protocol::external::types::Version;

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

/// A helper type to monitor the chain tip in order to determine the minimum peer protocol version
/// that is currently supported.
#[derive(Clone)]
pub struct MinimumPeerVersion<C> {
    network: Network,
    chain_tip: C,
    current_minimum: Version,
}

impl<C> fmt::Debug for MinimumPeerVersion<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // skip the chain tip to avoid locking issues
        f.debug_struct(std::any::type_name::<MinimumPeerVersion<C>>())
            .field("network", &self.network)
            .field("current_minimum", &self.current_minimum)
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
        }
    }

    /// Retrieve the current minimum supported peer protocol version.
    pub fn current(&mut self) -> Version {
        self.current_minimum =
            Version::min_remote_for_height(&self.network, self.chain_tip.best_tip_height());
        self.current_minimum
    }

    /// Returns a reference to the underlying chain tip.
    pub fn chain_tip(&self) -> &C {
        &self.chain_tip
    }
}
