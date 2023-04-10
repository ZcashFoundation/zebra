//! Prioritizing outbound peer connections based on peer attributes.

use std::net::SocketAddr;

use zebra_chain::parameters::Network;

use AttributePreference::*;

/// A level of preference for a peer attribute.
///
/// Invalid peer attributes are represented as errors.
///
/// Outbound peer connections are initiated in the sorted [order](std::cmp::Ord)
/// of this type.
///
/// The derived order depends on the order of the variants in the enum.
/// The variants are sorted in the order they are listed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum AttributePreference {
    /// This peer is more likely to be a valid Zcash network peer.
    ///
    /// # Correctness
    ///
    /// This variant must be declared as the first enum variant,
    /// so that `Preferred` peers sort before `Acceptable` peers.
    Preferred,

    /// This peer is possibly a valid Zcash network peer.
    Acceptable,
}

/// A level of preference for a peer.
///
/// Outbound peer connections are initiated in the sorted [order](std::cmp::Ord)
/// of this type.
///
/// The derived order depends on the order of the fields in the struct.
/// The first field determines the overall order, then later fields sort equal first field values.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PeerPreference {
    /// Is the peer using the canonical Zcash port for the configured [`Network`]?
    canonical_port: AttributePreference,
}

impl AttributePreference {
    /// Returns `Preferred` if `is_preferred` is true.
    pub fn preferred_from(is_preferred: bool) -> Self {
        if is_preferred {
            Preferred
        } else {
            Acceptable
        }
    }

    /// Returns `true` for `Preferred` attributes.
    #[allow(dead_code)]
    pub fn is_preferred(&self) -> bool {
        match self {
            Preferred => true,
            Acceptable => false,
        }
    }
}

impl PeerPreference {
    /// Return a preference for the peer at `peer_addr` on `network`.
    ///
    /// Use the [`PeerPreference`] [`Ord`] implementation to sort preferred peers first.
    pub fn new(
        peer_addr: &SocketAddr,
        network: impl Into<Option<Network>>,
    ) -> Result<PeerPreference, &'static str> {
        address_is_valid_for_outbound_connections(peer_addr, network)?;

        // This check only prefers the configured network,
        // because the address book and initial peer connections reject the port used by the other network.
        let canonical_port =
            AttributePreference::preferred_from([8232, 18232].contains(&peer_addr.port()));

        Ok(PeerPreference { canonical_port })
    }
}

/// Is the [`SocketAddr`] we have for this peer valid for outbound
/// connections?
///
/// Since the addresses in the address book are unique, this check can be
/// used to permanently reject entire [`MetaAddr`]s.
///
/// [`MetaAddr`]: crate::meta_addr::MetaAddr
fn address_is_valid_for_outbound_connections(
    peer_addr: &SocketAddr,
    network: impl Into<Option<Network>>,
) -> Result<(), &'static str> {
    // TODO: make private IP addresses an error unless a debug config is set (#3117)

    if peer_addr.ip().is_unspecified() {
        return Err("invalid peer IP address: unspecified addresses can not be used for outbound connections");
    }

    // 0 is an invalid outbound port.
    if peer_addr.port() == 0 {
        return Err(
            "invalid peer port: unspecified ports can not be used for outbound connections",
        );
    }

    // Ignore ports used by similar networks: Flux/ZelCash and misconfigured Zcash.
    if let Some(network) = network.into() {
        if peer_addr.port() == network.default_port() {
            return Ok(());
        }

        if peer_addr.port() == 8232 {
            return Err(
                "invalid peer port: port is for Mainnet, but this node is configured for Testnet",
            );
        } else if peer_addr.port() == 18232 {
            return Err(
                "invalid peer port: port is for Testnet, but this node is configured for Mainnet",
            );
        } else if peer_addr.port() == 18344 {
            return Err(
                "invalid peer port: port is for Regtest, but Zebra does not support that network",
            );
        } else if [16125, 26125].contains(&peer_addr.port()) {
            // 16125/26125 is used by Flux/ZelCash, which uses the same network magic numbers as Zcash,
            // so we have to reject it by port
            return Err("invalid peer port: port is for a non-Zcash network");
        }
    }

    Ok(())
}
