//! Records this process's memory use, so operators can see a node approaching its memory limit
//! before the kernel stops it.

use std::time::Duration;

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
/// memory limit, or the machine's memory if the cgroup has no lower limit.
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
    let cgroup = read("/sys/fs/cgroup/memory.max")
        .or_else(|| read("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
        .and_then(|limit| cgroup_limit(&limit));
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

    #[test]
    fn records_this_process_on_linux() {
        if cfg!(target_os = "linux") {
            let rss = read("/proc/self/status").and_then(|status| kib_field(&status, "VmRSS:"));
            assert!(rss.is_some_and(|rss| rss > 0));
        }
    }
}
