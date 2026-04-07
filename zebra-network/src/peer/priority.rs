//! Prioritizing outbound peer connections based on peer attributes.

use std::net::{IpAddr, SocketAddr};

use zebra_chain::parameters::Network;

use crate::PeerSocketAddr;

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
        peer_addr: impl Into<PeerSocketAddr>,
        network: impl Into<Option<Network>>,
        allow_private_ips: bool,
    ) -> Result<PeerPreference, &'static str> {
        let peer_addr = peer_addr.into();

        address_is_valid_for_outbound_connections(peer_addr, network, allow_private_ips)?;

        // This check only prefers the configured network, because
        // address_is_valid_for_outbound_connections() rejects the port used by the other network.
        let canonical_port =
            AttributePreference::preferred_from([8232, 18232].contains(&peer_addr.port()));

        Ok(PeerPreference { canonical_port })
    }
}

/// Returns `true` if `ip` is a private, loopback, link-local, or otherwise
/// non-globally-routable address.
///
/// Uses only stable standard library APIs (compatible with MSRV 1.85).
///
/// Covers:
/// - IPv4: RFC 1918 private (10/8, 172.16/12, 192.168/16), loopback (127/8), link-local (169.254/16)
/// - IPv6: loopback (::1), unique-local (fc00::/7), link-local unicast (fe80::/10)
fn ip_is_non_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let octets = v6.octets();
            // ::1 loopback
            v6.is_loopback()
            // fc00::/7 unique-local (covers 0xfc and 0xfd prefixes)
            || (octets[0] & 0xfe) == 0xfc
            // fe80::/10 link-local unicast
            || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
        }
    }
}

/// Is the [`PeerSocketAddr`] we have for this peer valid for outbound
/// connections?
///
/// Since the addresses in the address book are unique, this check can be
/// used to permanently reject entire [`MetaAddr`]s.
///
/// [`MetaAddr`]: crate::meta_addr::MetaAddr
pub fn address_is_valid_for_outbound_connections(
    peer_addr: PeerSocketAddr,
    network: impl Into<Option<Network>>,
    allow_private_ips: bool,
) -> Result<(), &'static str> {
    if peer_addr.ip().is_unspecified() {
        return Err("invalid peer IP address: unspecified addresses can not be used for outbound connections");
    }

    // 0 is an invalid outbound port.
    if peer_addr.port() == 0 {
        return Err(
            "invalid peer port: unspecified ports can not be used for outbound connections",
        );
    }

    // Regtest peers are always on loopback, so private IPs are allowed there by design.
    let network = network.into();
    let is_regtest = network.as_ref().map(|n| n.is_regtest()).unwrap_or(false);

    // Reject private, loopback, link-local, and other non-globally-routable addresses,
    // unless the debug config flag is set or this is regtest.
    //
    // # Security
    //
    // Connecting to or advertising private IP addresses allows Zebra to be used to probe
    // internal networks (SSRF) and disclose whether internal hosts run Zcash nodes.
    if !allow_private_ips && !is_regtest && ip_is_non_global(peer_addr.ip()) {
        return Err(
            "invalid peer IP address: private, loopback, and link-local addresses are not \
             allowed (set debug_allow_private_ip_addresses = true to override)",
        );
    }

    address_is_valid_for_inbound_listeners(*peer_addr, network)
}

/// Is the supplied [`SocketAddr`] valid for inbound listeners on `network`?
///
/// This is used to check Zebra's configured Zcash listener port.
pub fn address_is_valid_for_inbound_listeners(
    listener_addr: SocketAddr,
    network: impl Into<Option<Network>>,
) -> Result<(), &'static str> {
    // Ignore ports used by potentially compatible nodes: misconfigured Zcash ports.
    if let Some(network) = network.into() {
        if listener_addr.port() == network.default_port() {
            return Ok(());
        }

        if listener_addr.port() == 8232 {
            return Err(
                "invalid peer port: port is for Mainnet, but this node is configured for Testnet",
            );
        } else if listener_addr.port() == 18232 {
            return Err(
                "invalid peer port: port is for Testnet, but this node is configured for Mainnet",
            );
        }
    }

    // Ignore ports used by potentially compatible nodes: other coins and unsupported Zcash regtest.
    if listener_addr.port() == 18344 {
        return Err(
            "invalid peer port: port is for Regtest, but Zebra does not support that network",
        );
    } else if [8033, 18033, 16125, 26125].contains(&listener_addr.port()) {
        // These coins use the same network magic numbers as Zcash, so we have to reject them by port:
        // - ZClassic: 8033/18033
        //   https://github.com/ZclassicCommunity/zclassic/blob/504362bbf72400f51acdba519e12707da44138c2/src/chainparams.cpp#L130
        // - Flux/ZelCash: 16125/26125
        return Err("invalid peer port: port is for a non-Zcash coin");
    }

    Ok(())
}
