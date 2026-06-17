//! Version-pinned peer profiles used by Zakura compatibility tests.

use crate::{
    protocol::external::types::PeerServices,
    zakura::{
        select_zakura_protocol, ZakuraRejectReason, CONTROL_VERSION, P2P_V2_ALPN, PRELUDE_VERSION,
        ZAKURA_PROTOCOL_VERSION_1,
    },
};

/// Maximum ALPN identifiers retained for one pinned profile.
pub const MAX_PINNED_ALPNS: usize = 8;

/// One fixed historical peer profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerProfile {
    /// Plain Zebra: no Zakura service bit and no Zakura upgrade.
    PlainZebra,
    /// Real zcashd or a zcashd-like legacy peer.
    Zcashd,
    /// Zebra with Zakura compiled in but disabled.
    ZakuraDisabled,
    /// A Zakura peer pinned to one protocol/capability shape.
    Zakura(PinnedZakuraProfile),
}

impl PeerProfile {
    /// A pinned v1 Zakura peer.
    pub fn zakura_v1() -> Self {
        Self::Zakura(PinnedZakuraProfile::v1())
    }

    /// A synthetic v2-capable profile that still offers v1 for rolling upgrades.
    pub fn zakura_v2_offering_v1() -> Self {
        Self::Zakura(PinnedZakuraProfile {
            protocol_min: ZAKURA_PROTOCOL_VERSION_1,
            protocol_max: 2,
            prelude_version: PRELUDE_VERSION,
            control_version: CONTROL_VERSION,
            capabilities: 0,
            required_capabilities: 0,
            alpns: vec![b"p2p-v2/2".to_vec(), P2P_V2_ALPN.to_vec()],
        })
    }

    /// Human-readable matrix label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::PlainZebra => "plain-zebra",
            Self::Zcashd => "zcashd",
            Self::ZakuraDisabled => "zakura-disabled",
            Self::Zakura(profile) if profile.protocol_max == ZAKURA_PROTOCOL_VERSION_1 => {
                "zakura-v1"
            }
            Self::Zakura(_) => "zakura-v2",
        }
    }

    /// Services advertised in the legacy Zcash version message.
    pub fn advertised_services(&self) -> PeerServices {
        match self {
            Self::PlainZebra | Self::Zcashd | Self::ZakuraDisabled => PeerServices::NODE_NETWORK,
            Self::Zakura(_) => PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2,
        }
    }

    /// Returns true if this profile advertises the Zakura service bit.
    pub fn advertises_zakura(&self) -> bool {
        self.advertised_services()
            .contains(PeerServices::NODE_P2P_V2)
    }

    /// Returns the pinned Zakura profile, if enabled.
    pub fn zakura(&self) -> Option<&PinnedZakuraProfile> {
        match self {
            Self::Zakura(profile) => Some(profile),
            Self::PlainZebra | Self::Zcashd | Self::ZakuraDisabled => None,
        }
    }
}

/// A bounded Zakura protocol/capability profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedZakuraProfile {
    /// Lowest supported Zakura wire protocol.
    pub protocol_min: u16,
    /// Highest supported Zakura wire protocol.
    pub protocol_max: u16,
    /// Supported legacy-upgrade prelude version.
    pub prelude_version: u16,
    /// Supported control handshake version.
    pub control_version: u16,
    /// Optional capability bits this peer can use.
    pub capabilities: u64,
    /// Capability bits this peer requires from the remote.
    pub required_capabilities: u64,
    /// ALPNs offered by this profile, newest first.
    pub alpns: Vec<Vec<u8>>,
}

impl PinnedZakuraProfile {
    /// Returns the released v1 profile.
    pub fn v1() -> Self {
        Self {
            protocol_min: ZAKURA_PROTOCOL_VERSION_1,
            protocol_max: ZAKURA_PROTOCOL_VERSION_1,
            prelude_version: PRELUDE_VERSION,
            control_version: CONTROL_VERSION,
            capabilities: 0,
            required_capabilities: 0,
            alpns: vec![P2P_V2_ALPN.to_vec()],
        }
    }

    /// Validate local bounds before using this profile in a matrix run.
    pub fn validate(&self) -> Result<(), PinnedProfileError> {
        if self.protocol_min > self.protocol_max {
            return Err(PinnedProfileError::InvalidProtocolRange);
        }
        if self.prelude_version == 0 {
            return Err(PinnedProfileError::ZeroPreludeVersion);
        }
        if self.control_version == 0 {
            return Err(PinnedProfileError::ZeroControlVersion);
        }
        if self.alpns.is_empty() {
            return Err(PinnedProfileError::EmptyAlpns);
        }
        if self.alpns.len() > MAX_PINNED_ALPNS {
            return Err(PinnedProfileError::TooManyAlpns {
                actual: self.alpns.len(),
                max: MAX_PINNED_ALPNS,
            });
        }
        if self.required_capabilities & !self.capabilities != 0 {
            return Err(PinnedProfileError::RequiresUnsupportedCapability);
        }

        Ok(())
    }
}

/// A pinned-profile construction error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedProfileError {
    /// The protocol range is internally inconsistent.
    InvalidProtocolRange,
    /// Prelude version zero is not a valid released wire profile.
    ZeroPreludeVersion,
    /// Control version zero is not a valid released wire profile.
    ZeroControlVersion,
    /// A Zakura profile must offer at least one ALPN.
    EmptyAlpns,
    /// The profile exceeds the local ALPN list bound.
    TooManyAlpns {
        /// Actual ALPN count.
        actual: usize,
        /// Maximum supported ALPN count.
        max: usize,
    },
    /// The profile requires a capability it does not itself advertise.
    RequiresUnsupportedCapability,
}

/// A peer pinned to exactly one compatibility profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedPeer {
    profile: PeerProfile,
}

impl PinnedPeer {
    /// Create a pinned peer after validating profile-local bounds.
    pub fn new(profile: PeerProfile) -> Result<Self, PinnedProfileError> {
        if let Some(zakura) = profile.zakura() {
            zakura.validate()?;
        }

        Ok(Self { profile })
    }

    /// Borrow this peer's profile.
    pub fn profile(&self) -> &PeerProfile {
        &self.profile
    }

    /// Negotiate this peer with `remote`.
    pub fn negotiate(&self, remote: &PinnedPeer) -> PinnedNegotiation {
        match (self.profile.zakura(), remote.profile.zakura()) {
            (Some(local), Some(remote)) => {
                if local.required_capabilities & !remote.capabilities != 0
                    || remote.required_capabilities & !local.capabilities != 0
                {
                    return PinnedNegotiation::NeutralReject(
                        ZakuraRejectReason::MissingRequiredCapability,
                    );
                }

                let Some(alpn) = select_alpn(&local.alpns, &remote.alpns) else {
                    return PinnedNegotiation::NeutralReject(
                        ZakuraRejectReason::IncompatibleZakuraProtocol,
                    );
                };

                match select_zakura_protocol(
                    local.protocol_min,
                    local.protocol_max,
                    remote.protocol_min,
                    remote.protocol_max,
                ) {
                    Ok(selected_protocol) => PinnedNegotiation::Upgrade {
                        selected_protocol,
                        alpn,
                    },
                    Err(reason) => PinnedNegotiation::NeutralReject(reason),
                }
            }
            _ => PinnedNegotiation::LegacyOnly,
        }
    }
}

/// The outcome selected by pinned-profile negotiation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedNegotiation {
    /// Use only the legacy Zcash/Zebra protocol.
    LegacyOnly,
    /// Upgrade to Zakura.
    Upgrade {
        /// Highest mutually supported Zakura protocol version.
        selected_protocol: u16,
        /// Highest-preferred mutually supported ALPN.
        alpn: Vec<u8>,
    },
    /// Reject the Zakura upgrade without punishing the legacy peer.
    NeutralReject(ZakuraRejectReason),
}

fn select_alpn(local: &[Vec<u8>], remote: &[Vec<u8>]) -> Option<Vec<u8>> {
    local
        .iter()
        .find(|candidate| remote.iter().any(|remote_alpn| remote_alpn == *candidate))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_profiles_advertise_expected_services() {
        assert!(!PeerProfile::PlainZebra.advertises_zakura());
        assert!(!PeerProfile::Zcashd.advertises_zakura());
        assert!(!PeerProfile::ZakuraDisabled.advertises_zakura());
        assert!(PeerProfile::zakura_v1().advertises_zakura());
    }

    #[test]
    fn pinned_negotiation_selects_highest_mutual_version_and_alpn() {
        let v1 = PinnedPeer::new(PeerProfile::zakura_v1()).unwrap();
        let v2 = PinnedPeer::new(PeerProfile::zakura_v2_offering_v1()).unwrap();

        assert_eq!(
            v2.negotiate(&v1),
            PinnedNegotiation::Upgrade {
                selected_protocol: 1,
                alpn: P2P_V2_ALPN.to_vec(),
            }
        );

        assert_eq!(
            v2.negotiate(&v2),
            PinnedNegotiation::Upgrade {
                selected_protocol: 2,
                alpn: b"p2p-v2/2".to_vec(),
            }
        );
    }

    #[test]
    fn unknown_optional_capability_bits_do_not_block_negotiation() {
        let v1 = PinnedPeer::new(PeerProfile::zakura_v1()).unwrap();
        let future = PinnedPeer::new(PeerProfile::Zakura(PinnedZakuraProfile {
            capabilities: 1 << 63,
            ..PinnedZakuraProfile::v1()
        }))
        .unwrap();

        assert!(matches!(
            v1.negotiate(&future),
            PinnedNegotiation::Upgrade {
                selected_protocol: 1,
                ..
            }
        ));
    }

    #[test]
    fn missing_required_capability_rejects_neutrally() {
        let v1 = PinnedPeer::new(PeerProfile::zakura_v1()).unwrap();
        let required = PinnedPeer::new(PeerProfile::Zakura(PinnedZakuraProfile {
            capabilities: 1,
            required_capabilities: 1,
            ..PinnedZakuraProfile::v1()
        }))
        .unwrap();

        assert_eq!(
            v1.negotiate(&required),
            PinnedNegotiation::NeutralReject(ZakuraRejectReason::MissingRequiredCapability)
        );
    }

    #[test]
    fn unknown_alpns_reject_neutrally_without_panicking() {
        let v1 = PinnedPeer::new(PeerProfile::zakura_v1()).unwrap();
        let future_alpn = PinnedPeer::new(PeerProfile::Zakura(PinnedZakuraProfile {
            alpns: vec![b"p2p-v2/future".to_vec()],
            ..PinnedZakuraProfile::v1()
        }))
        .unwrap();

        assert_eq!(
            v1.negotiate(&future_alpn),
            PinnedNegotiation::NeutralReject(ZakuraRejectReason::IncompatibleZakuraProtocol)
        );
    }

    #[test]
    fn invalid_pinned_profiles_report_fixture_errors_not_wire_rejects() {
        let error = PinnedPeer::new(PeerProfile::Zakura(PinnedZakuraProfile {
            protocol_min: 2,
            protocol_max: 1,
            ..PinnedZakuraProfile::v1()
        }))
        .unwrap_err();

        assert_eq!(error, PinnedProfileError::InvalidProtocolRange);
    }
}
