//! Shared checks for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

/// The cache_dir config used in the ephemeral mode tests
#[derive(Debug, PartialEq, Eq)]
pub enum EphemeralConfig {
    /// the cache_dir config is left at its default value
    Default,
    /// the cache_dir config is set to a path in the tempdir
    MisconfiguredCacheDir,
}

/// The check performed by the ephemeral mode tests
#[derive(Debug, PartialEq, Eq)]
pub enum EphemeralCheck {
    /// an existing directory is not deleted
    ExistingDirectory,
    /// a missing directory is not created
    MissingDirectory,
}

/// Is `s` a valid `zebrad` version string?
///
/// Trims whitespace before parsing the version.
///
/// Returns false if the version is invalid, or if there is anything else on the
/// line that contains the version. In particular, this check will fail if `s`
/// includes any terminal formatting.
pub fn is_zebrad_version(s: &str) -> bool {
    semver::Version::parse(s.replace("zebrad", "").trim()).is_ok()
}
