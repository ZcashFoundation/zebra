//! Embedded zcashd compatibility release manifest.
//!
//! [`zebrad/zcashd-compat-manifest.json`](../../../zcashd-compat-manifest.json) is the
//! single source of truth for the zcashd compat pin: CI and Docker builds read it via
//! `scripts/resolve-zcashd-compat-manifest.sh`, and zebrad embeds it at compile time
//! and parses it here.

use std::sync::LazyLock;

use serde::Deserialize;

/// Embedded manifest schema version.
pub const EMBEDDED_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Embedded zcashd compatibility release manifest.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZcashdReleaseManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Release tag from which artifacts were published.
    pub release_tag: String,
    /// Release artifacts by target triple.
    pub artifacts: Vec<ZcashdReleaseArtifact>,
}

/// One released zcashd compatibility artifact.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZcashdReleaseArtifact {
    /// Rust-style target triple.
    pub target_triple: String,
    /// Fully-qualified runtime archive URL.
    pub runtime_archive_url: String,
    /// SHA256 hex digest of the runtime archive.
    pub runtime_archive_sha256: String,
    /// Runtime archive member path that points to the `zcashd` executable.
    pub runtime_archive_member_binary_path: String,
}

/// Embedded manifest used by managed zcashd downloads.
pub static EMBEDDED_ZCASHD_RELEASE_MANIFEST: LazyLock<ZcashdReleaseManifest> =
    LazyLock::new(|| {
        let manifest: ZcashdReleaseManifest =
            serde_json::from_str(include_str!("../../../zcashd-compat-manifest.json"))
                .expect("committed zcashd-compat-manifest.json is validated by unit tests and CI");

        assert_eq!(
            manifest.schema_version, EMBEDDED_MANIFEST_SCHEMA_VERSION,
            "unsupported zcashd-compat-manifest.json schema version"
        );

        manifest
    });

impl ZcashdReleaseManifest {
    /// Returns the configured artifact for `target_triple`, if any.
    pub fn artifact_for_target(&self, target_triple: &str) -> Option<&ZcashdReleaseArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.target_triple == target_triple)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::EMBEDDED_ZCASHD_RELEASE_MANIFEST;

    #[test]
    fn embedded_manifest_targets_are_unique() {
        let mut targets = HashSet::new();
        for artifact in &EMBEDDED_ZCASHD_RELEASE_MANIFEST.artifacts {
            assert!(
                targets.insert(artifact.target_triple.as_str()),
                "duplicate manifest target found: {}",
                artifact.target_triple
            );
        }
    }

    #[test]
    fn embedded_manifest_entries_are_well_formed() {
        for artifact in &EMBEDDED_ZCASHD_RELEASE_MANIFEST.artifacts {
            assert!(
                artifact.runtime_archive_url.starts_with("https://"),
                "managed zcashd artifact URL must be https: {}",
                artifact.runtime_archive_url
            );
            assert_eq!(
                artifact.runtime_archive_sha256.len(),
                64,
                "artifact SHA256 must be 64 hex chars for target {}",
                artifact.target_triple
            );
            assert!(
                artifact
                    .runtime_archive_sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit()),
                "artifact SHA256 contains non-hex characters for target {}",
                artifact.target_triple
            );
        }
    }
}
