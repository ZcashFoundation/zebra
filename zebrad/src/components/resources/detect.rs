//! Detection of the memory available to this process, for `memory_budget = "auto"`.

#[cfg(target_os = "linux")]
use std::{
    fs,
    path::{Path, PathBuf},
};

use color_eyre::{eyre::eyre, Report};

#[cfg(test)]
mod tests;

/// Returns the memory available to this process, in bytes: the smaller of the host's total
/// memory and this process's cgroup memory limit.
///
/// # Errors
///
/// If the available memory cannot be detected, for example on a platform without
/// `/proc/meminfo`.
pub fn detect() -> Result<u64, Report> {
    // Integration scaffold: the budget core package moves the `/proc/meminfo` and cgroup parsers
    // here from the zcashd-compat preflight.
    Err(eyre!(
        "could not detect the memory available to Zebra: set [resources] memory_budget to a size, \
         like \"2 GiB\""
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn meminfo_total_bytes() -> Result<u64, Report> {
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
pub(crate) fn cgroup_memory_limit_bytes() -> Result<Option<u64>, Report> {
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
