//! Embedded zcashd compatibility release manifest.
//!
//! This file is intended to be generated from zcash release metadata.
//! It is currently maintained manually until the generation step lands.

/// Embedded manifest schema version.
pub const EMBEDDED_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Embedded zcashd compatibility release manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZcashdReleaseManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Release tag from which artifacts were published.
    pub release_tag: &'static str,
    /// Release artifacts by target triple.
    pub artifacts: &'static [ZcashdReleaseArtifact],
}

/// One released zcashd compatibility artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZcashdReleaseArtifact {
    /// Rust-style target triple.
    pub target_triple: &'static str,
    /// Fully-qualified runtime archive URL.
    pub runtime_archive_url: &'static str,
    /// SHA256 hex digest of the runtime archive.
    pub runtime_archive_sha256: &'static str,
    /// Runtime archive member path that points to the `zcashd` executable.
    pub runtime_archive_member_binary_path: &'static str,
}

/// Embedded manifest used by managed zcashd downloads.
///
pub const EMBEDDED_ZCASHD_RELEASE_MANIFEST: ZcashdReleaseManifest = ZcashdReleaseManifest {
    schema_version: EMBEDDED_MANIFEST_SCHEMA_VERSION,
    release_tag: "v6.2.1-alpha",
    artifacts: &[
        ZcashdReleaseArtifact {
            target_triple: "x86_64-pc-linux-gnu",
            runtime_archive_url: "https://github.com/valargroup/zcashd/releases/download/v6.2.1-alpha/zcashd-zebra-compat-v6.2.1-alpha-linux-x86_64.tar.gz",
            runtime_archive_sha256: "09e640b55c9af91dee5742e5e9bb6712f92d7073f0fe899ca58d43f62eb9d13c",
            runtime_archive_member_binary_path: "./bin/zcashd",
        },
        ZcashdReleaseArtifact {
            target_triple: "aarch64-linux-gnu",
            runtime_archive_url: "https://github.com/valargroup/zcashd/releases/download/v6.2.1-alpha/zcashd-zebra-compat-v6.2.1-alpha-linux-aarch64.tar.gz",
            runtime_archive_sha256: "133e562f86b4f49dd4e69ad55eb94bdd259c5e8af4fc533ec72b1f39bbbb6927",
            runtime_archive_member_binary_path: "./bin/zcashd",
        },
    ],
};

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
        for artifact in EMBEDDED_ZCASHD_RELEASE_MANIFEST.artifacts {
            assert!(
                targets.insert(artifact.target_triple),
                "duplicate manifest target found: {}",
                artifact.target_triple
            );
        }
    }

    #[test]
    fn embedded_manifest_entries_are_well_formed() {
        for artifact in EMBEDDED_ZCASHD_RELEASE_MANIFEST.artifacts {
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
