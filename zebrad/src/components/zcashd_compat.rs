//! zcashd-compat mode configuration and `zcashd` child-process supervision.

mod config;
mod datadir;
mod managed;
mod manifest;
mod preflight;
mod supervisor;

pub use config::{Config, ZcashdBinarySource as ConfigZcashdBinarySource};
pub use datadir::{effective_zcashd_datadir, ensure_zcashd_datadir, resolve_zcashd_datadir_path};
pub use managed::{
    effective_zcashd_source, resolve_managed_zcashd_binary, resolve_zcashd_binary_path,
    zcashd_target_triple, ZcashdBinarySource,
};
pub use manifest::{
    ZcashdReleaseArtifact, ZcashdReleaseManifest, EMBEDDED_MANIFEST_SCHEMA_VERSION,
    EMBEDDED_ZCASHD_RELEASE_MANIFEST,
};
pub use preflight::run_preflight;
pub use supervisor::{
    is_command_resolvable, reject_peer_selection_extra_args, run as run_supervisor,
    set_supervision_config_disabled_metrics, set_supervision_unexpectedly_disabled_metrics,
    SupervisorConfig,
};
