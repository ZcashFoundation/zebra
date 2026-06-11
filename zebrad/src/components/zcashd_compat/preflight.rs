//! Linux hardware preflight checks for zcashd-compat mode.

#[cfg(target_os = "linux")]
use std::{
    collections::HashMap,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    thread::available_parallelism,
};

#[cfg(target_os = "linux")]
use color_eyre::eyre::{eyre, Report};
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use tracing::warn;

#[cfg(target_os = "linux")]
use super::{effective_zcashd_datadir, ensure_zcashd_datadir, resolve_zcashd_datadir_path};
use crate::config::ZebradConfig;

#[cfg(target_os = "linux")]
const GIB: u64 = 1024 * 1024 * 1024;
#[cfg(target_os = "linux")]
const TIB: u64 = 1024 * GIB;

#[cfg(target_os = "linux")]
const MIN_CPU_LOGICAL: usize = 4;
#[cfg(target_os = "linux")]
const RECOMMENDED_CPU_LOGICAL: usize = 8;

#[cfg(target_os = "linux")]
const MIN_RAM_BYTES: u64 = 16 * GIB;
#[cfg(target_os = "linux")]
const RECOMMENDED_RAM_BYTES: u64 = 32 * GIB;

#[cfg(target_os = "linux")]
const MIN_ZEBRA_PROVISIONED_BYTES: u64 = 300 * GIB;
#[cfg(target_os = "linux")]
const MIN_ZCASHD_PROVISIONED_BYTES: u64 = 300 * GIB;
#[cfg(target_os = "linux")]
const RECOMMENDED_COMBINED_TOTAL_BYTES: u64 = TIB;

/// Runs zcashd-compat hardware preflight checks.
///
/// On Linux, checks CPU, effective memory and mount-aware disk provisioning.
/// On non-Linux, startup fails unless `unsafe_low_specs` is explicitly set.
pub fn run_preflight(
    config: &ZebradConfig,
    unsafe_low_specs: bool,
) -> Result<(), color_eyre::eyre::Report> {
    #[cfg(target_os = "linux")]
    {
        run_linux_preflight(config, unsafe_low_specs)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, unsafe_low_specs);
        let message = "zcashd-compat mode is supported on Linux only";

        if unsafe_low_specs {
            tracing::warn!(
                "{message}. continuing because --unsafe-low-specs was explicitly provided"
            );
            Ok(())
        } else {
            Err(color_eyre::eyre::eyre!(message))
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum DiskRole {
    ZebraState,
    ZcashdData,
}

#[cfg(target_os = "linux")]
impl DiskRole {
    fn label(self) -> &'static str {
        match self {
            DiskRole::ZebraState => "zebra state",
            DiskRole::ZcashdData => "zcashd datadir",
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
struct PathRequirement {
    role: DiskRole,
    target_path: PathBuf,
    min_provisioned_bytes: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Default)]
struct FilesystemRequirements {
    roles: Vec<DiskRole>,
    target_paths: Vec<PathBuf>,
    min_provisioned_sum_bytes: u64,
    provisioned_bytes: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Default)]
struct PreflightSummary {
    errors: Vec<String>,
    warnings: Vec<String>,
}

#[cfg(target_os = "linux")]
fn run_linux_preflight(config: &ZebradConfig, unsafe_low_specs: bool) -> Result<(), Report> {
    let mut zcashd_datadir =
        effective_zcashd_datadir(&config.zcashd_compat, &config.state.cache_dir);

    if config.zcashd_compat.manage_zcashd {
        zcashd_datadir =
            resolve_zcashd_datadir_path(&zcashd_datadir, &config.zcashd_compat.zcashd_extra_args);
        ensure_zcashd_datadir(&zcashd_datadir, &config.zcashd_compat.zcashd_extra_args)?;
    }

    let mut summary = PreflightSummary::default();
    check_cpu(&mut summary)?;
    check_memory(&mut summary)?;
    check_disk(&mut summary, &config.state.cache_dir, &zcashd_datadir)?;

    for warning in finalize_preflight(summary, unsafe_low_specs)? {
        warn!("{warning}");
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn finalize_preflight(
    mut summary: PreflightSummary,
    unsafe_low_specs: bool,
) -> Result<Vec<String>, Report> {
    if !summary.errors.is_empty() {
        if unsafe_low_specs {
            summary
                .warnings
                .extend(summary.errors.into_iter().map(|error| {
                    format!(
                        "{error}. continuing because --unsafe-low-specs was explicitly provided"
                    )
                }));
        } else {
            return Err(eyre!(
                "zcashd-compat preflight failed:\n- {}",
                summary.errors.join("\n- ")
            ));
        }
    }

    Ok(summary.warnings)
}

#[cfg(target_os = "linux")]
fn check_cpu(summary: &mut PreflightSummary) -> Result<(), Report> {
    let cpu_count = available_parallelism()
        .map(NonZeroUsize::get)
        .map_err(|error| eyre!("failed to read available logical CPU count: {error}"))?;

    if cpu_count < MIN_CPU_LOGICAL {
        summary.errors.push(format!(
            "detected {cpu_count} logical CPUs, minimum required is {MIN_CPU_LOGICAL}"
        ));
    } else if cpu_count < RECOMMENDED_CPU_LOGICAL {
        summary.warnings.push(format!(
            "detected {cpu_count} logical CPUs, recommended is {RECOMMENDED_CPU_LOGICAL}"
        ));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn check_memory(summary: &mut PreflightSummary) -> Result<(), Report> {
    let mem_total = meminfo_total_bytes()?;
    let cgroup_limit = cgroup_memory_limit_bytes()?;
    let effective_memory = cgroup_limit.map_or(mem_total, |limit| limit.min(mem_total));

    if effective_memory < MIN_RAM_BYTES {
        summary.errors.push(format!(
            "detected effective memory {}, minimum required is {}",
            human_gib(effective_memory),
            human_gib(MIN_RAM_BYTES)
        ));
    } else if effective_memory < RECOMMENDED_RAM_BYTES {
        summary.warnings.push(format!(
            "detected effective memory {}, recommended is {}",
            human_gib(effective_memory),
            human_gib(RECOMMENDED_RAM_BYTES)
        ));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn check_disk(
    summary: &mut PreflightSummary,
    zebra_cache_dir: &Path,
    zcashd_datadir: &Path,
) -> Result<(), Report> {
    let requirements = vec![
        PathRequirement {
            role: DiskRole::ZebraState,
            target_path: zebra_cache_dir.to_path_buf(),
            min_provisioned_bytes: MIN_ZEBRA_PROVISIONED_BYTES,
        },
        PathRequirement {
            role: DiskRole::ZcashdData,
            target_path: zcashd_datadir.to_path_buf(),
            min_provisioned_bytes: MIN_ZCASHD_PROVISIONED_BYTES,
        },
    ];

    let grouped_filesystems = grouped_requirements_by_filesystem(&requirements)?;
    evaluate_disk_thresholds(summary, &grouped_filesystems);

    Ok(())
}

#[cfg(target_os = "linux")]
fn evaluate_disk_thresholds(
    summary: &mut PreflightSummary,
    grouped_filesystems: &HashMap<u64, FilesystemRequirements>,
) {
    let combined_provisioned_capacity = grouped_filesystems
        .values()
        .map(|filesystem| filesystem.provisioned_bytes)
        .sum::<u64>();

    for filesystem in grouped_filesystems.values() {
        if filesystem.provisioned_bytes < filesystem.min_provisioned_sum_bytes {
            summary.errors.push(format!(
                "{} mount (paths: {}) has provisioned capacity {}, minimum required is {}",
                role_labels(&filesystem.roles),
                display_paths(&filesystem.target_paths),
                human_gib(filesystem.provisioned_bytes),
                human_gib(filesystem.min_provisioned_sum_bytes),
            ));
        }
    }

    if combined_provisioned_capacity < RECOMMENDED_COMBINED_TOTAL_BYTES {
        summary.warnings.push(format!(
            "combined zcashd-compat filesystem capacity is {}, recommended is {}",
            human_gib(combined_provisioned_capacity),
            human_gib(RECOMMENDED_COMBINED_TOTAL_BYTES)
        ));
    }
}

#[cfg(target_os = "linux")]
fn grouped_requirements_by_filesystem(
    requirements: &[PathRequirement],
) -> Result<HashMap<u64, FilesystemRequirements>, Report> {
    let mut grouped = HashMap::new();

    for requirement in requirements {
        let probed_path = nearest_existing_ancestor(&requirement.target_path)?;
        let metadata = fs::metadata(&probed_path).map_err(|error| {
            eyre!(
                "failed to read metadata for {}: {error}",
                probed_path.display()
            )
        })?;
        let device_id = metadata.dev();
        let provisioned_bytes = statvfs_provisioned_bytes(&probed_path)?;

        let entry = grouped
            .entry(device_id)
            .or_insert_with(|| FilesystemRequirements {
                provisioned_bytes,
                ..FilesystemRequirements::default()
            });

        if !entry.roles.contains(&requirement.role) {
            entry.roles.push(requirement.role);
        }
        entry.target_paths.push(requirement.target_path.clone());
        entry.min_provisioned_sum_bytes = entry
            .min_provisioned_sum_bytes
            .saturating_add(requirement.min_provisioned_bytes);
    }

    Ok(grouped)
}

#[cfg(target_os = "linux")]
fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf, Report> {
    let mut current = path.to_path_buf();

    loop {
        if current.exists() {
            return Ok(current);
        }

        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
            continue;
        }

        return Err(eyre!(
            "no existing ancestor path found for {}",
            path.display()
        ));
    }
}

#[cfg(target_os = "linux")]
fn statvfs_provisioned_bytes(path: &Path) -> Result<u64, Report> {
    let stats = nix::sys::statvfs::statvfs(path).map_err(|error| {
        eyre!(
            "failed to get filesystem stats for {}: {error}",
            path.display()
        )
    })?;

    let fragment_size = stats.fragment_size();

    Ok(stats.blocks().saturating_mul(fragment_size))
}

#[cfg(target_os = "linux")]
fn meminfo_total_bytes() -> Result<u64, Report> {
    let meminfo = fs::read_to_string("/proc/meminfo")
        .map_err(|error| eyre!("failed to read /proc/meminfo: {error}"))?;
    let mem_total_kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| eyre!("MemTotal field missing in /proc/meminfo"))?
        .parse::<u64>()
        .map_err(|error| eyre!("failed to parse MemTotal from /proc/meminfo: {error}"))?;

    Ok(mem_total_kib.saturating_mul(1024))
}

#[cfg(target_os = "linux")]
/// Returns the effective cgroup memory limit for the current process, in bytes.
///
/// Specification:
/// - Read `/proc/self/cgroup` and extract:
///   - the cgroup v2 relative path (`0::...`) when present, and
///   - the cgroup v1 `memory` controller relative path (`...:memory:...`) when present.
/// - Probe candidate limit files in this order:
///   - v2: process-specific `/sys/fs/cgroup/<path>/memory.max`, then root fallback
///     `/sys/fs/cgroup/memory.max`;
///   - v1: process-specific
///     `/sys/fs/cgroup/memory/<path>/memory.limit_in_bytes`, then root fallback
///     `/sys/fs/cgroup/memory/memory.limit_in_bytes`.
/// - Missing files are treated as unavailable and skipped.
/// - Unlimited values (`max` in v2, very large sentinel in v1) are treated as `None`.
/// - If both v1 and v2 limits are available, return the tighter (`min`) limit.
/// - If only one limit is available, return that limit; if neither is available, return `None`.
fn cgroup_memory_limit_bytes() -> Result<Option<u64>, Report> {
    let self_cgroup = fs::read_to_string("/proc/self/cgroup")
        .map_err(|error| eyre!("failed to read /proc/self/cgroup: {error}"))?;
    let (v2_relative_path, v1_memory_relative_path) = parse_self_cgroup_paths(&self_cgroup);

    let v2_limit = parse_first_cgroup_limit(
        &v2_relative_path
            .iter()
            .map(|relative_path| {
                cgroup_limit_path(Path::new("/sys/fs/cgroup"), relative_path, "memory.max")
            })
            .chain(std::iter::once(PathBuf::from("/sys/fs/cgroup/memory.max")))
            .collect::<Vec<_>>(),
    )?;

    let v1_limit = parse_first_cgroup_limit(
        &v1_memory_relative_path
            .iter()
            .map(|relative_path| {
                cgroup_limit_path(
                    Path::new("/sys/fs/cgroup/memory"),
                    relative_path,
                    "memory.limit_in_bytes",
                )
            })
            .chain(std::iter::once(PathBuf::from(
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
            )))
            .collect::<Vec<_>>(),
    )?;

    Ok(select_cgroup_memory_limit(v2_limit, v1_limit))
}

#[cfg(target_os = "linux")]
fn parse_self_cgroup_paths(self_cgroup: &str) -> (Option<String>, Option<String>) {
    let mut v2_relative_path = None;
    let mut v1_memory_relative_path = None;

    for line in self_cgroup.lines() {
        let mut fields = line.splitn(3, ':');
        let _hierarchy_id = fields.next();
        let controllers = fields.next();
        let relative_path = fields.next();

        let (Some(controllers), Some(relative_path)) = (controllers, relative_path) else {
            continue;
        };

        if controllers.is_empty() {
            v2_relative_path = Some(relative_path.to_string());
        } else if controllers
            .split(',')
            .any(|controller| controller == "memory")
        {
            v1_memory_relative_path = Some(relative_path.to_string());
        }
    }

    (v2_relative_path, v1_memory_relative_path)
}

#[cfg(target_os = "linux")]
fn cgroup_limit_path(base_path: &Path, relative_path: &str, file_name: &str) -> PathBuf {
    let normalized_relative_path = relative_path.trim_start_matches('/');

    if normalized_relative_path.is_empty() {
        base_path.join(file_name)
    } else {
        base_path.join(normalized_relative_path).join(file_name)
    }
}

#[cfg(target_os = "linux")]
fn parse_first_cgroup_limit(candidate_paths: &[PathBuf]) -> Result<Option<u64>, Report> {
    for path in candidate_paths {
        if let Some(limit) = parse_cgroup_limit(path)? {
            return Ok(Some(limit));
        }
    }

    Ok(None)
}

#[cfg(target_os = "linux")]
fn parse_cgroup_limit(path: &Path) -> Result<Option<u64>, Report> {
    let raw_limit = match fs::read_to_string(path) {
        Ok(raw_limit) => raw_limit,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(eyre!("failed to read {}: {error}", path.display())),
    };

    let trimmed = raw_limit.trim();
    if trimmed.eq_ignore_ascii_case("max") {
        return Ok(None);
    }

    let parsed_limit = trimmed.parse::<u64>().map_err(|error| {
        eyre!(
            "failed to parse cgroup memory limit from {}: {error}",
            path.display()
        )
    })?;

    // cgroup v1 can report very large sentinel values for "unlimited".
    if parsed_limit >= 0x7fff_ffff_ffff_f000 {
        return Ok(None);
    }

    Ok(Some(parsed_limit))
}

#[cfg(target_os = "linux")]
fn select_cgroup_memory_limit(v2_limit: Option<u64>, v1_limit: Option<u64>) -> Option<u64> {
    match (v2_limit, v1_limit) {
        (Some(v2), Some(v1)) => Some(v2.min(v1)),
        (Some(v2), None) => Some(v2),
        (None, Some(v1)) => Some(v1),
        (None, None) => None,
    }
}

#[cfg(target_os = "linux")]
fn role_labels(roles: &[DiskRole]) -> String {
    roles
        .iter()
        .map(|role| role.label())
        .collect::<Vec<_>>()
        .join(" + ")
}

#[cfg(target_os = "linux")]
fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(target_os = "linux")]
fn human_gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn merges_provisioned_requirement_when_paths_share_filesystem() {
        let requirements = vec![
            PathRequirement {
                role: DiskRole::ZebraState,
                target_path: PathBuf::from("/tmp"),
                min_provisioned_bytes: MIN_ZEBRA_PROVISIONED_BYTES,
            },
            PathRequirement {
                role: DiskRole::ZcashdData,
                target_path: PathBuf::from("/tmp"),
                min_provisioned_bytes: MIN_ZCASHD_PROVISIONED_BYTES,
            },
        ];

        let grouped = grouped_requirements_by_filesystem(&requirements)
            .expect("filesystem grouping should succeed");
        let filesystem = grouped.values().next().expect("group should not be empty");

        assert_eq!(
            filesystem.min_provisioned_sum_bytes,
            MIN_ZEBRA_PROVISIONED_BYTES + MIN_ZCASHD_PROVISIONED_BYTES
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_cgroup_max_as_unlimited() {
        assert_eq!(parse_cgroup_value("max").expect("valid cgroup value"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_cgroup_numeric_value() {
        assert_eq!(
            parse_cgroup_value("17179869184").expect("valid cgroup value"),
            Some(17_179_869_184)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn prefers_v1_when_v2_is_unavailable() {
        assert_eq!(select_cgroup_memory_limit(None, Some(16)), Some(16));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn chooses_tighter_limit_when_both_are_available() {
        assert_eq!(select_cgroup_memory_limit(Some(32), Some(16)), Some(16));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_v2_and_v1_process_cgroup_paths() {
        let cgroup = "0::/user.slice/user-1000.slice/session-2.scope\n2:memory:/docker/abcdef";
        let (v2_path, v1_path) = parse_self_cgroup_paths(cgroup);

        assert_eq!(
            v2_path,
            Some("/user.slice/user-1000.slice/session-2.scope".to_string())
        );
        assert_eq!(v1_path, Some("/docker/abcdef".to_string()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn builds_cgroup_limit_paths_with_root_and_nested_relative_paths() {
        assert_eq!(
            cgroup_limit_path(Path::new("/sys/fs/cgroup"), "/", "memory.max"),
            PathBuf::from("/sys/fs/cgroup/memory.max")
        );
        assert_eq!(
            cgroup_limit_path(
                Path::new("/sys/fs/cgroup/memory"),
                "/docker/abcdef",
                "memory.limit_in_bytes"
            ),
            PathBuf::from("/sys/fs/cgroup/memory/docker/abcdef/memory.limit_in_bytes")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reports_disk_failure_when_below_minimum_provisioned_capacity() {
        let mut summary = PreflightSummary::default();
        let mut grouped = HashMap::new();
        grouped.insert(
            1,
            FilesystemRequirements {
                roles: vec![DiskRole::ZebraState, DiskRole::ZcashdData],
                target_paths: vec!["/zebra".into(), "/zcashd".into()],
                min_provisioned_sum_bytes: MIN_ZEBRA_PROVISIONED_BYTES
                    + MIN_ZCASHD_PROVISIONED_BYTES,
                provisioned_bytes: 400 * GIB,
            },
        );

        evaluate_disk_thresholds(&mut summary, &grouped);

        assert_eq!(summary.errors.len(), 1);
        assert!(
            summary
                .errors
                .iter()
                .any(|error| error.contains("provisioned capacity")),
            "expected provisioned capacity error: {:?}",
            summary.errors
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bypass_turns_failures_into_warnings() {
        let summary = PreflightSummary {
            errors: vec!["cpu below minimum".to_string()],
            warnings: vec!["disk below recommendation".to_string()],
        };

        let warnings = finalize_preflight(summary, true).expect("unsafe bypass should continue");

        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("--unsafe-low-specs")),
            "expected unsafe bypass warning message: {warnings:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fails_when_below_minimum_without_bypass() {
        let summary = PreflightSummary {
            errors: vec!["ram below minimum".to_string()],
            warnings: Vec::new(),
        };

        let error = finalize_preflight(summary, false)
            .expect_err("preflight should fail without unsafe bypass");
        assert!(
            error.to_string().contains("preflight failed"),
            "unexpected error: {error}"
        );
    }

    #[cfg(target_os = "linux")]
    fn parse_cgroup_value(value: &str) -> Result<Option<u64>, Report> {
        if value.trim().eq_ignore_ascii_case("max") {
            return Ok(None);
        }

        let parsed_limit = value
            .trim()
            .parse::<u64>()
            .map_err(|error| eyre!("failed to parse cgroup memory limit: {error}"))?;

        if parsed_limit >= 0x7fff_ffff_ffff_f000 {
            return Ok(None);
        }

        Ok(Some(parsed_limit))
    }
}
