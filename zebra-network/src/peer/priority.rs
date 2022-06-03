//! Priorising outbound peer connections based on peer attributes.

use std::net::SocketAddr;

use zebra_chain::parameters::Network;

use AttributePreference::*;

/// A level of preference for a peer attribute.
///
/// Invalid peer attributes are represented as errors.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum AttributePreference {
    /// This peer is more likely to be a valid Zcash network peer.
    Preferred,

    /// This peer is possibly a valid Zcash network peer.
    _Acceptable,
}

/// A level of preference for a peer.
///
/// Outbound peer connections are initiated in the sorted [order](std::ops::Ord) of this type.
///
/// In particular, public addresses and non-canonical ports are preferred to
/// private addresses and canonical ports.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PeerPreference {
    /// Does the peer have a publicly routable IP address?
    ///
    /// TODO: make private addresses an error unless a debug config is set (#3117)
    public_address: AttributePreference,

    /// Is the peer using the canonical Zcash port for the configured [`Network`]?
    canonical_port: AttributePreference,
}

/// Return a preference for the peer at `peer_addr` on `network`.
///
/// Use the [`PeerPreference`] [`Ord`] implementation to sort preferred peers first.
pub fn peer_preference(
    peer_addr: SocketAddr,
    _network: Network,
) -> Result<PeerPreference, &'static str> {
    if !address_is_valid_for_outbound_connections(peer_addr) {
        return Err("invalid address: can not be used for outbound connections");
    }

    Ok(PeerPreference {
        public_address: Preferred,
        canonical_port: Preferred,
    })
}

/// Is the [`SocketAddr`] we have for this peer valid for outbound
/// connections?
///
/// Since the addresses in the address book are unique, this check can be
/// used to permanently reject entire [`MetaAddr`]s.
fn address_is_valid_for_outbound_connections(peer_addr: SocketAddr) -> bool {
    if peer_addr.ip().is_unspecified() {
        return false;
    }

    if peer_addr.port() == 0 {
        return false;
    }

    true
}
