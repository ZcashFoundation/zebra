//! zcashd-compat mode configuration and `zcashd` child-process supervision.

mod config;
mod managed;
mod manifest;
mod preflight;
mod supervisor;

pub use config::{Config, ZcashdBinarySource as ConfigZcashdBinarySource};
pub use managed::{
    effective_zcashd_source, resolve_managed_zcashd_binary, resolve_zcashd_binary_path,
    zcashd_target_triple, ZcashdBinarySource,
};
pub use manifest::{
    ZcashdReleaseArtifact, ZcashdReleaseManifest, EMBEDDED_MANIFEST_SCHEMA_VERSION,
    EMBEDDED_ZCASHD_RELEASE_MANIFEST,
};
pub use preflight::run_preflight;
pub use supervisor::{is_command_resolvable, run as run_supervisor, SupervisorConfig};
