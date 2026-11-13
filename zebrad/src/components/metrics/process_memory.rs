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
    let cgroup = tightest_cgroup_limit(&read("/proc/self/cgroup").unwrap_or_default(), |path| {
        std::fs::read_to_string(path).ok()
    });
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
/// `/proc/self/cgroup`.
///
/// A service usually runs in a nested cgroup, such as a systemd unit under `system.slice`, and any
/// cgroup between it and the root can set the limit that applies. So this checks the process's
/// cgroup and every ancestor, under both the cgroup v2 and the v1 `memory` hierarchies.
#[cfg(target_os = "linux")]
fn tightest_cgroup_limit(self_cgroup: &str, read: impl Fn(&Path) -> Option<String>) -> Option<u64> {
    let (v2_path, v1_path) = parse_self_cgroup_paths(self_cgroup);

    limit_files("/sys/fs/cgroup", v2_path.as_deref(), "memory.max")
        .into_iter()
        .chain(limit_files(
            "/sys/fs/cgroup/memory",
            v1_path.as_deref(),
            "memory.limit_in_bytes",
        ))
        .filter_map(|path| read(&path))
        .filter_map(|limit| cgroup_limit(&limit))
        .min()
}

/// Returns the paths of `file_name` in the cgroup at `relative_path` under `mount`, and in each of
/// its ancestors up to the mount root.
///
/// A path that leaves the mount (`..`, seen when the cgroup is outside this cgroup namespace) only
/// checks the mount root.
#[cfg(target_os = "linux")]
fn limit_files(mount: &str, relative_path: Option<&str>, file_name: &str) -> Vec<PathBuf> {
    let mount = Path::new(mount);
    let relative_path = Path::new(relative_path.unwrap_or("/").trim_start_matches('/'));
    let cgroup = if relative_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        mount.join(relative_path)
    } else {
        mount.to_path_buf()
    };

    cgroup
        .ancestors()
        .take_while(|dir| dir.starts_with(mount))
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
            tightest_cgroup_limit(self_cgroup, |path| v2.get(path).cloned()),
            Some(4 * gib)
        );

        // A tighter limit on the service itself wins over its slice.
        let mut v2_service = v2.clone();
        v2_service.insert(
            PathBuf::from("/sys/fs/cgroup/system.slice/zebrad.service/memory.max"),
            format!("{}\n", 2 * gib),
        );
        assert_eq!(
            tightest_cgroup_limit(self_cgroup, |path| v2_service.get(path).cloned()),
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
            tightest_cgroup_limit(self_cgroup, |path| v1.get(path).cloned()),
            Some(3 * gib)
        );

        // Without a readable `/proc/self/cgroup`, or with a cgroup outside this namespace, only the
        // root limit is checked.
        let root = files(&[("/sys/fs/cgroup/memory.max", format!("{}\n", 6 * gib))]);
        assert_eq!(
            tightest_cgroup_limit("", |path| root.get(path).cloned()),
            Some(6 * gib)
        );
        assert_eq!(
            limit_files("/sys/fs/cgroup", Some("/../../user.slice"), "memory.max"),
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
