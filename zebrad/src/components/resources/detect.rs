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
/// memory and this process's cgroup memory limit (design §2.2).
///
/// # Errors
///
/// If the available memory cannot be detected. Detection reads `/proc` and `/sys/fs/cgroup`, so
/// it is only supported on Linux; elsewhere `"auto"` asks for an explicit budget (D9).
pub fn detect() -> Result<u64, Report> {
    #[cfg(target_os = "linux")]
    {
        MemoryFiles::default()
            .effective_memory_bytes()
            .map_err(|error| {
                eyre!(
                    "could not detect the memory available to Zebra: {error}: set [resources] \
                     memory_budget to a size, like \"2 GiB\""
                )
            })
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err(eyre!(
            "memory_budget = \"auto\" can only detect the available memory on Linux: set \
             [resources] memory_budget to a size, like \"2 GiB\""
        ))
    }
}

/// The files the memory detection reads.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MemoryFiles {
    /// The host's memory statistics, `/proc/meminfo`.
    pub meminfo: PathBuf,

    /// This process's cgroups, `/proc/self/cgroup`.
    pub self_cgroup: PathBuf,

    /// The cgroup filesystem's mount point, `/sys/fs/cgroup`.
    pub cgroup_root: PathBuf,
}

#[cfg(target_os = "linux")]
impl Default for MemoryFiles {
    fn default() -> Self {
        Self {
            meminfo: PathBuf::from("/proc/meminfo"),
            self_cgroup: PathBuf::from("/proc/self/cgroup"),
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
        }
    }
}

#[cfg(target_os = "linux")]
impl MemoryFiles {
    /// Returns the memory available to this process, in bytes: the smaller of the host's total
    /// memory and this process's cgroup memory limit.
    pub fn effective_memory_bytes(&self) -> Result<u64, Report> {
        let mem_total = meminfo_total_bytes_at(&self.meminfo)?;
        let cgroup_limit = cgroup_memory_limit_bytes_at(&self.self_cgroup, &self.cgroup_root)?;

        Ok(cgroup_limit.map_or(mem_total, |limit| limit.min(mem_total)))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn meminfo_total_bytes() -> Result<u64, Report> {
    meminfo_total_bytes_at(&MemoryFiles::default().meminfo)
}

/// Returns the `MemTotal` of the `/proc/meminfo` file at `path`, in bytes.
#[cfg(target_os = "linux")]
fn meminfo_total_bytes_at(path: &Path) -> Result<u64, Report> {
    let meminfo = fs::read_to_string(path)
        .map_err(|error| eyre!("failed to read {}: {error}", path.display()))?;
    let mem_total_kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| eyre!("MemTotal field missing in {}", path.display()))?
        .parse::<u64>()
        .map_err(|error| eyre!("failed to parse MemTotal from {}: {error}", path.display()))?;

    Ok(mem_total_kib.saturating_mul(1024))
}

#[cfg(target_os = "linux")]
/// Returns the effective cgroup memory limit for the current process, in bytes.
///
/// Specification:
/// - Read `/proc/self/cgroup` and extract:
///   - the cgroup v2 relative path (`0::...`) when present, and
///   - the cgroup v1 `memory` controller relative path (`...:memory:...`) when present.
/// - Walk from the process's cgroup through every ancestor to the root, separately for:
///   - v2: `/sys/fs/cgroup/<path>/memory.max`;
///   - v1: `/sys/fs/cgroup/memory/<path>/memory.limit_in_bytes`.
/// - Missing files are treated as unavailable and skipped.
/// - Unlimited values (`max` in v2, very large sentinel in v1) are treated as `None`.
/// - Return the minimum finite limit across every ancestor and both cgroup versions, or `None`
///   if every entry is missing or unlimited.
pub(crate) fn cgroup_memory_limit_bytes() -> Result<Option<u64>, Report> {
    let files = MemoryFiles::default();

    cgroup_memory_limit_bytes_at(&files.self_cgroup, &files.cgroup_root)
}

/// Returns the cgroup memory limit of the process whose `/proc/self/cgroup` is the file at
/// `self_cgroup_path`, with the cgroup filesystem mounted at `cgroup_root`, in bytes.
///
/// See [`cgroup_memory_limit_bytes`] for the files it probes.
#[cfg(target_os = "linux")]
fn cgroup_memory_limit_bytes_at(
    self_cgroup_path: &Path,
    cgroup_root: &Path,
) -> Result<Option<u64>, Report> {
    let self_cgroup = fs::read_to_string(self_cgroup_path)
        .map_err(|error| eyre!("failed to read {}: {error}", self_cgroup_path.display()))?;
    let (v2_relative_path, v1_memory_relative_path) = parse_self_cgroup_paths(&self_cgroup);
    let v1_memory_root = cgroup_root.join("memory");

    let v2_limit = parse_cgroup_ancestor_limit(
        cgroup_root,
        v2_relative_path.as_deref().unwrap_or("/"),
        "memory.max",
    )?;
    let v1_limit = parse_cgroup_ancestor_limit(
        &v1_memory_root,
        v1_memory_relative_path.as_deref().unwrap_or("/"),
        "memory.limit_in_bytes",
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

/// Returns the smallest finite limit in the leaf-to-root chain, skipping missing and unlimited
/// entries. The number of reads is bounded by the number of path components in `/proc/self/cgroup`.
#[cfg(target_os = "linux")]
fn parse_cgroup_ancestor_limit(
    base_path: &Path,
    relative_path: &str,
    file_name: &str,
) -> Result<Option<u64>, Report> {
    let mut tightest: Option<u64> = None;
    let mut ancestor = relative_path.trim_matches('/');

    loop {
        let path = cgroup_limit_path(base_path, ancestor, file_name);
        if let Some(limit) = parse_cgroup_limit(&path)? {
            tightest = Some(match tightest {
                Some(current) => current.min(limit),
                None => limit,
            });
        }

        if ancestor.is_empty() {
            break;
        }
        ancestor = ancestor
            .rsplit_once('/')
            .map_or("", |(parent, _leaf)| parent);
    }

    Ok(tightest)
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
