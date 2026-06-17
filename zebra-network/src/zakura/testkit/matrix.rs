//! Compatibility matrix runner for pinned Zakura peer profiles.

use super::{PeerProfile, PinnedNegotiation, PinnedPeer};
use crate::zakura::ZakuraRejectReason;
#[cfg(test)]
use crate::zakura::P2P_V2_ALPN;

/// Profile ids used by the checked-in compatibility table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProfileId {
    /// Zebra with Zakura disabled.
    ZakuraDisabled,
    /// Released Zakura v1.
    ZakuraV1,
    /// Synthetic v2 profile that still offers v1.
    ZakuraV2,
    /// Plain Zebra peer.
    PlainZebra,
}

impl ProfileId {
    fn profile(self) -> PeerProfile {
        match self {
            Self::ZakuraDisabled => PeerProfile::ZakuraDisabled,
            Self::ZakuraV1 => PeerProfile::zakura_v1(),
            Self::ZakuraV2 => PeerProfile::zakura_v2_offering_v1(),
            Self::PlainZebra => PeerProfile::PlainZebra,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ZakuraDisabled => "zakura-disabled",
            Self::ZakuraV1 => "zakura-v1",
            Self::ZakuraV2 => "zakura-v2",
            Self::PlainZebra => "plain-zebra",
        }
    }
}

/// Expected result for one matrix cell.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExpectedOutcome {
    /// Remain on the legacy Zcash/Zebra network path only.
    LegacyOnly,
    /// Upgrade and select this Zakura protocol version.
    Upgrade(u16),
    /// Reject the Zakura upgrade neutrally.
    NeutralReject(ZakuraRejectReason),
}

/// One checked matrix cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatrixCell {
    /// Local profile label.
    pub local: &'static str,
    /// Remote profile label.
    pub remote: &'static str,
    /// Expected result.
    pub expected: ExpectedOutcome,
    /// Actual result.
    pub actual: PinnedNegotiation,
}

/// Profiles covered by the first compatibility matrix.
pub fn default_profiles() -> Vec<PeerProfile> {
    vec![
        ProfileId::ZakuraDisabled.profile(),
        ProfileId::ZakuraV1.profile(),
        ProfileId::ZakuraV2.profile(),
    ]
}

/// Remote profiles covered by the first compatibility matrix.
pub fn default_remote_profiles() -> Vec<PeerProfile> {
    vec![
        ProfileId::PlainZebra.profile(),
        ProfileId::ZakuraDisabled.profile(),
        ProfileId::ZakuraV1.profile(),
        ProfileId::ZakuraV2.profile(),
    ]
}

/// One literal expected-outcome table row.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MatrixExpectation {
    /// Local profile id.
    pub local: ProfileId,
    /// Remote profile id.
    pub remote: ProfileId,
    /// Expected outcome.
    pub expected: ExpectedOutcome,
}

const DEFAULT_EXPECTATIONS: &[MatrixExpectation] = &[
    MatrixExpectation {
        local: ProfileId::ZakuraDisabled,
        remote: ProfileId::PlainZebra,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraDisabled,
        remote: ProfileId::ZakuraDisabled,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraDisabled,
        remote: ProfileId::ZakuraV1,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraDisabled,
        remote: ProfileId::ZakuraV2,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV1,
        remote: ProfileId::PlainZebra,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV1,
        remote: ProfileId::ZakuraDisabled,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV1,
        remote: ProfileId::ZakuraV1,
        expected: ExpectedOutcome::Upgrade(1),
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV1,
        remote: ProfileId::ZakuraV2,
        expected: ExpectedOutcome::Upgrade(1),
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV2,
        remote: ProfileId::PlainZebra,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV2,
        remote: ProfileId::ZakuraDisabled,
        expected: ExpectedOutcome::LegacyOnly,
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV2,
        remote: ProfileId::ZakuraV1,
        expected: ExpectedOutcome::Upgrade(1),
    },
    MatrixExpectation {
        local: ProfileId::ZakuraV2,
        remote: ProfileId::ZakuraV2,
        expected: ExpectedOutcome::Upgrade(2),
    },
];

/// Expected outcome table from the compatibility contract.
pub fn expected_outcomes() -> &'static [MatrixExpectation] {
    DEFAULT_EXPECTATIONS
}

/// Run the path-independent negotiation matrix and return all checked cells.
pub fn run_default_matrix() -> Vec<MatrixCell> {
    let mut cells = Vec::new();

    for expectation in DEFAULT_EXPECTATIONS {
        let local_peer =
            PinnedPeer::new(expectation.local.profile()).expect("default local profile is valid");
        let remote_peer =
            PinnedPeer::new(expectation.remote.profile()).expect("default remote profile is valid");
        cells.push(MatrixCell {
            local: expectation.local.label(),
            remote: expectation.remote.label(),
            expected: expectation.expected,
            actual: local_peer.negotiate(&remote_peer),
        });
    }

    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_matrix_matches_expected_outcome_table() {
        for cell in run_default_matrix() {
            match (&cell.expected, &cell.actual) {
                (ExpectedOutcome::LegacyOnly, PinnedNegotiation::LegacyOnly) => {}
                (
                    ExpectedOutcome::Upgrade(expected_version),
                    PinnedNegotiation::Upgrade {
                        selected_protocol,
                        alpn,
                    },
                ) => {
                    assert_eq!(
                        selected_protocol, expected_version,
                        "wrong selected version for {} -> {}",
                        cell.local, cell.remote,
                    );
                    assert!(
                        alpn == P2P_V2_ALPN || alpn == b"p2p-v2/2",
                        "unexpected ALPN for {} -> {}: {:?}",
                        cell.local,
                        cell.remote,
                        String::from_utf8_lossy(alpn),
                    );
                }
                (
                    ExpectedOutcome::NeutralReject(expected_reason),
                    PinnedNegotiation::NeutralReject(actual_reason),
                ) => assert_eq!(actual_reason, expected_reason),
                _ => panic!(
                    "unexpected matrix outcome for {} -> {}: expected {:?}, got {:?}",
                    cell.local, cell.remote, cell.expected, cell.actual,
                ),
            }
        }
    }

    #[test]
    fn legacy_only_cells_do_not_advertise_a_mutual_upgrade() {
        for cell in run_default_matrix()
            .into_iter()
            .filter(|cell| cell.expected == ExpectedOutcome::LegacyOnly)
        {
            assert_eq!(
                cell.actual,
                PinnedNegotiation::LegacyOnly,
                "legacy-only matrix cell leaked Zakura upgrade: {cell:?}",
            );
        }
    }

    #[test]
    fn neutral_reject_outcome_is_representable_and_checked() {
        let local = PinnedPeer::new(PeerProfile::zakura_v1()).unwrap();
        let required = PinnedPeer::new(PeerProfile::Zakura(super::super::PinnedZakuraProfile {
            capabilities: 1,
            required_capabilities: 1,
            ..super::super::PinnedZakuraProfile::v1()
        }))
        .unwrap();
        let expected =
            ExpectedOutcome::NeutralReject(ZakuraRejectReason::MissingRequiredCapability);

        assert_eq!(
            expected,
            ExpectedOutcome::NeutralReject(ZakuraRejectReason::MissingRequiredCapability)
        );
        assert_eq!(
            local.negotiate(&required),
            PinnedNegotiation::NeutralReject(ZakuraRejectReason::MissingRequiredCapability)
        );
    }
}
