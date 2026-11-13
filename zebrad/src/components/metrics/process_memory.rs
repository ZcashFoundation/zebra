//! Records this process's memory use, so operators can see a node approaching its memory limit
//! before the kernel stops it.

#[cfg(target_os = "linux")]
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::components::zcashd_compat::parse_self_cgroup_paths;

/// How often [`record_process_memory`] samples memory use.
///
/// Shorter than the Prometheus scrape and Sentry flush intervals, so each one sees a recent value.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(15);

/// Records this process's memory use every [`SAMPLE_INTERVAL`], until the task is aborted.
pub async fn record_process_memory() {
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    loop {
        interval.tick().await;
        record();
    }
}

/// Sets the `process.memory.rss_bytes` gauge to the resident set size of this process, and
/// `process.memory.rss_fraction` to that size as a fraction of the memory it can use: the cgroup
/// memory limit, or the machine's memory if no cgroup sets a lower limit.
///
/// Does nothing on platforms without `/proc`, or if the values can't be read.
pub(crate) fn record() {
    let Some(rss) = read("/proc/self/status").and_then(|status| kib_field(&status, "VmRSS:"))
    else {
        return;
    };
    // Memory sizes are far below the integers f64 represents exactly.
    metrics::gauge!("process.memory.rss_bytes").set(rss as f64);

    let machine = read("/proc/meminfo").and_then(|meminfo| kib_field(&meminfo, "MemTotal:"));
    #[cfg(target_os = "linux")]
    let cgroup = tightest_cgroup_limit(
        &read("/proc/self/cgroup").unwrap_or_default(),
        &read("/proc/self/mountinfo").unwrap_or_default(),
        |path| std::fs::read_to_string(path).ok(),
    );
    #[cfg(not(target_os = "linux"))]
    let cgroup = None;
    if let Some(available) = available_memory(machine, cgroup) {
        metrics::gauge!("process.memory.rss_fraction").set(rss as f64 / available as f64);
    }
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Returns the value in bytes of a `name  <value> kB` line, as in `/proc/self/status` and
/// `/proc/meminfo`.
fn kib_field(contents: &str, name: &str) -> Option<u64> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(name))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

/// Returns the tightest memory limit on this process's cgroups, given the contents of
/// `/proc/self/cgroup` and `/proc/self/mountinfo`.
///
/// A service usually runs in a nested cgroup, such as a systemd unit under `system.slice`, and any
/// cgroup between it and the root can set the limit that applies. So this checks the process's
/// cgroup and every ancestor visible in a mount, under both the cgroup v2 and the v1 `memory`
/// hierarchies.
#[cfg(target_os = "linux")]
fn tightest_cgroup_limit(
    self_cgroup: &str,
    mountinfo: &str,
    read: impl Fn(&Path) -> Option<String>,
) -> Option<u64> {
    let (v2_path, v1_path) = parse_self_cgroup_paths(self_cgroup);
    let (mut v2_mounts, mut v1_mounts) = cgroup_mounts(mountinfo);
    if v2_mounts.is_empty() && v1_mounts.is_empty() {
        v2_mounts.push(CgroupMount::at_root("/sys/fs/cgroup"));
        v1_mounts.push(CgroupMount::at_root("/sys/fs/cgroup/memory"));
    }

    limit_files(&v2_mounts, v2_path.as_deref(), "memory.max")
        .into_iter()
        .chain(limit_files(
            &v1_mounts,
            v1_path.as_deref(),
            "memory.limit_in_bytes",
        ))
        .filter_map(|path| read(&path))
        .filter_map(|limit| cgroup_limit(&limit))
        .min()
}

/// A mounted cgroup hierarchy.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct CgroupMount {
    /// The cgroup in the hierarchy that the mount shows at its mount point. Membership paths in
    /// `/proc/self/cgroup` are relative to the hierarchy, not to this cgroup.
    root: PathBuf,
    /// Where the cgroup at `root` appears in the filesystem.
    mount_point: PathBuf,
}

#[cfg(target_os = "linux")]
impl CgroupMount {
    fn at_root(mount_point: &str) -> Self {
        Self {
            root: PathBuf::from("/"),
            mount_point: PathBuf::from(mount_point),
        }
    }
}

/// Returns the cgroup v2 mounts and the cgroup v1 `memory` mounts listed in
/// `/proc/self/mountinfo`.
#[cfg(target_os = "linux")]
fn cgroup_mounts(mountinfo: &str) -> (Vec<CgroupMount>, Vec<CgroupMount>) {
    let mut v2 = Vec::new();
    let mut v1_memory = Vec::new();

    for line in mountinfo.lines() {
        // Optional fields come before the ` - ` separator, so the mount fields are counted from
        // the start and the filesystem fields from the separator.
        let Some((mount, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        let mut mount = mount.split_whitespace().skip(3);
        let (Some(root), Some(mount_point)) = (mount.next(), mount.next()) else {
            continue;
        };
        let mut filesystem = filesystem.split_whitespace();
        let (Some(fs_type), Some(_source), super_options) =
            (filesystem.next(), filesystem.next(), filesystem.next())
        else {
            continue;
        };
        let cgroup_mount = CgroupMount {
            root: PathBuf::from(unescape_mountinfo(root)),
            mount_point: PathBuf::from(unescape_mountinfo(mount_point)),
        };

        if fs_type == "cgroup2" {
            v2.push(cgroup_mount);
        } else if fs_type == "cgroup"
            && super_options.is_some_and(|options| options.split(',').any(|o| o == "memory"))
        {
            v1_memory.push(cgroup_mount);
        }
    }

    (v2, v1_memory)
}

/// Decodes the octal escapes (such as `\040` for a space) that `/proc/self/mountinfo` uses in
/// paths.
#[cfg(target_os = "linux")]
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escape = bytes.get(i + 1..i + 4).filter(|digits| {
            bytes[i] == b'\\' && digits.iter().all(|digit| (b'0'..=b'7').contains(digit))
        });
        match escape
            .and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok())
        {
            Some(byte) => {
                decoded.push(byte);
                i += 4;
            }
            None => {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Returns the paths of `file_name` in the cgroup at `membership` and in each of its ancestors
/// that the mount with the deepest matching root shows.
///
/// A membership path that leaves the hierarchy (`..`, seen when the cgroup is outside this cgroup
/// namespace) only checks the mount points.
#[cfg(target_os = "linux")]
fn limit_files(mounts: &[CgroupMount], membership: Option<&str>, file_name: &str) -> Vec<PathBuf> {
    let membership = Path::new(membership.unwrap_or("/"));
    if !membership
        .components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return mounts
            .iter()
            .map(|mount| mount.mount_point.join(file_name))
            .collect();
    }

    let Some((mount, relative_path)) = mounts
        .iter()
        .filter_map(|mount| Some((mount, membership.strip_prefix(&mount.root).ok()?)))
        .max_by_key(|(mount, _)| mount.root.components().count())
    else {
        return Vec::new();
    };

    mount
        .mount_point
        .join(relative_path)
        .ancestors()
        .take_while(|dir| dir.starts_with(&mount.mount_point))
        .map(|dir| dir.join(file_name))
        .collect()
}

/// Returns a cgroup memory limit in bytes, or `None` if the cgroup has no limit.
///
/// cgroup v2 writes `max` for no limit. cgroup v1 writes a value near `i64::MAX`, which is larger
/// than any machine's memory, so [`available_memory`] ignores it.
fn cgroup_limit(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

/// Returns the memory this process can use: the smaller of the machine's memory and the cgroup
/// limit, ignoring values that are unknown or zero.
fn available_memory(machine: Option<u64>, cgroup: Option<u64>) -> Option<u64> {
    machine
        .into_iter()
        .chain(cgroup)
        .filter(|bytes| *bytes > 0)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_kib_fields_as_bytes() {
        let status = "Name:\tzebrad\nVmHWM:\t 7067912 kB\nVmRSS:\t 6886020 kB\nThreads:\t34\n";

        assert_eq!(kib_field(status, "VmRSS:"), Some(6_886_020 * 1024));
        assert_eq!(
            kib_field("MemTotal:       8131200 kB\n", "MemTotal:"),
            Some(8_131_200 * 1024)
        );
        assert_eq!(kib_field(status, "MemTotal:"), None);
    }

    #[test]
    fn uses_the_smaller_of_the_machine_and_cgroup_limits() {
        let gib = 1024 * 1024 * 1024;

        assert_eq!(cgroup_limit("max\n"), None);
        assert_eq!(cgroup_limit("4294967296\n"), Some(4 * gib));

        assert_eq!(available_memory(Some(8 * gib), None), Some(8 * gib));
        assert_eq!(
            available_memory(Some(8 * gib), Some(4 * gib)),
            Some(4 * gib)
        );
        // cgroup v1 reports no limit as a value near i64::MAX.
        assert_eq!(
            available_memory(Some(8 * gib), Some(9_223_372_036_854_771_712)),
            Some(8 * gib)
        );
        assert_eq!(available_memory(None, Some(0)), None);
    }

    #[cfg(target_os = "linux")]
    const V2_MOUNTINFO: &str =
        "25 30 0:22 / /sys/fs/cgroup rw,nosuid shared:9 - cgroup2 cgroup2 rw,nsdelegate\n";

    #[cfg(target_os = "linux")]
    const V1_MOUNTINFO: &str = "\
        30 25 0:26 / /sys/fs/cgroup/cpu,cpuacct rw shared:10 - cgroup cgroup rw,cpu,cpuacct\n\
        31 25 0:27 / /sys/fs/cgroup/memory rw shared:11 - cgroup cgroup rw,memory\n";

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_cgroup_mounts_in_mountinfo() {
        let mountinfo = "\
            22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
            25 22 0:22 /tenant /mnt/tenant\\040cgroups rw shared:9 master:2 - cgroup2 cgroup2 rw\n\
            31 22 0:27 /docker /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n\
            32 22 0:28 / /sys/fs/cgroup/pids rw - cgroup cgroup rw,pids\n";

        let (v2, v1_memory) = cgroup_mounts(mountinfo);

        assert_eq!(
            v2,
            vec![CgroupMount {
                root: PathBuf::from("/tenant"),
                mount_point: PathBuf::from("/mnt/tenant cgroups"),
            }]
        );
        assert_eq!(
            v1_memory,
            vec![CgroupMount {
                root: PathBuf::from("/docker"),
                mount_point: PathBuf::from("/sys/fs/cgroup/memory"),
            }]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uses_the_mount_root_to_find_cgroups_in_a_subtree_mount() {
        let gib: u64 = 1024 * 1024 * 1024;
        // The mount shows the `/tenant` cgroup at `/sys/fs/cgroup`, so the process's `/tenant/zebrad`
        // cgroup is at `/sys/fs/cgroup/zebrad`, not `/sys/fs/cgroup/tenant/zebrad`.
        let mountinfo = "25 30 0:22 /tenant /sys/fs/cgroup rw shared:9 - cgroup2 cgroup2 rw\n";
        let files: std::collections::HashMap<_, _> = [
            (
                PathBuf::from("/sys/fs/cgroup/memory.max"),
                format!("{}\n", 8 * gib),
            ),
            (
                PathBuf::from("/sys/fs/cgroup/zebrad/memory.max"),
                format!("{}\n", 2 * gib),
            ),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            tightest_cgroup_limit("0::/tenant/zebrad\n", mountinfo, |path| files
                .get(path)
                .cloned()),
            Some(2 * gib)
        );

        // A cgroup outside the mounted subtree can't be seen, so it has no visible limit.
        assert_eq!(
            tightest_cgroup_limit("0::/other/zebrad\n", mountinfo, |path| files
                .get(path)
                .cloned()),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uses_the_tightest_limit_on_nested_cgroups() {
        let gib: u64 = 1024 * 1024 * 1024;
        let files = |entries: &[(&str, String)]| {
            entries
                .iter()
                .map(|(path, value)| (PathBuf::from(path), value.clone()))
                .collect::<std::collections::HashMap<_, _>>()
        };

        // cgroup v2: the service has no limit of its own, but its slice does.
        let v2 = files(&[
            ("/sys/fs/cgroup/memory.max", "max\n".into()),
            (
                "/sys/fs/cgroup/system.slice/memory.max",
                format!("{}\n", 4 * gib),
            ),
            (
                "/sys/fs/cgroup/system.slice/zebrad.service/memory.max",
                "max\n".into(),
            ),
        ]);
        let self_cgroup = "0::/system.slice/zebrad.service\n";
        assert_eq!(
            tightest_cgroup_limit(self_cgroup, V2_MOUNTINFO, |path| v2.get(path).cloned()),
            Some(4 * gib)
        );

        // A tighter limit on the service itself wins over its slice.
        let mut v2_service = v2.clone();
        v2_service.insert(
            PathBuf::from("/sys/fs/cgroup/system.slice/zebrad.service/memory.max"),
            format!("{}\n", 2 * gib),
        );
        assert_eq!(
            tightest_cgroup_limit(self_cgroup, V2_MOUNTINFO, |path| {
                v2_service.get(path).cloned()
            }),
            Some(2 * gib)
        );

        // cgroup v1: the limit is on the process's `memory` cgroup, not the hierarchy root.
        let v1 = files(&[
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "9223372036854771712\n".into(),
            ),
            (
                "/sys/fs/cgroup/memory/docker/abcdef/memory.limit_in_bytes",
                format!("{}\n", 3 * gib),
            ),
        ]);
        let self_cgroup = "12:cpu,cpuacct:/docker/abcdef\n4:memory:/docker/abcdef\n";
        assert_eq!(
            tightest_cgroup_limit(self_cgroup, V1_MOUNTINFO, |path| v1.get(path).cloned()),
            Some(3 * gib)
        );

        // Without readable `/proc/self/cgroup` and `/proc/self/mountinfo`, or with a cgroup outside
        // this namespace, only the root limit is checked.
        let root = files(&[("/sys/fs/cgroup/memory.max", format!("{}\n", 6 * gib))]);
        assert_eq!(
            tightest_cgroup_limit("", "", |path| root.get(path).cloned()),
            Some(6 * gib)
        );
        assert_eq!(
            limit_files(
                &[CgroupMount::at_root("/sys/fs/cgroup")],
                Some("/../../user.slice"),
                "memory.max"
            ),
            vec![PathBuf::from("/sys/fs/cgroup/memory.max")]
        );
    }

    #[test]
    fn records_this_process_on_linux() {
        if cfg!(target_os = "linux") {
            let rss = read("/proc/self/status").and_then(|status| kib_field(&status, "VmRSS:"));
            assert!(rss.is_some_and(|rss| rss > 0));
        }
    }
}
