//! Tests for the meminfo and cgroup memory limit parsers.

#[cfg(target_os = "linux")]
use super::*;

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

/// A fake `/proc` and `/sys/fs/cgroup` in a temporary directory.
#[cfg(target_os = "linux")]
struct FakeHost {
    dir: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl FakeHost {
    /// Returns a host with `mem_total_kib` of memory and `self_cgroup` as its
    /// `/proc/self/cgroup`.
    fn new(mem_total_kib: u64, self_cgroup: &str) -> Self {
        let host = Self {
            dir: tempfile::tempdir().expect("a temporary directory can be created"),
        };
        host.write(
            "proc/meminfo",
            &format!("MemTotal:       {mem_total_kib} kB\nMemFree:         1024 kB\n"),
        );
        host.write("proc/self/cgroup", self_cgroup);
        host
    }

    /// Writes `contents` to the file at `path` under this host's root.
    fn write(&self, path: &str, contents: &str) {
        let path = self.dir.path().join(path);
        std::fs::create_dir_all(path.parent().expect("every fixture file has a parent"))
            .expect("fixture directories can be created");
        std::fs::write(path, contents).expect("fixture files can be written");
    }

    /// Returns the detection's files under this host's root.
    fn files(&self) -> MemoryFiles {
        MemoryFiles {
            meminfo: self.dir.path().join("proc/meminfo"),
            self_cgroup: self.dir.path().join("proc/self/cgroup"),
            cgroup_root: self.dir.path().join("sys/fs/cgroup"),
        }
    }
}

#[cfg(target_os = "linux")]
const GIB_KIB: u64 = 1024 * 1024;

#[cfg(target_os = "linux")]
const GIB: u64 = 1024 * GIB_KIB;

/// A systemd scope's cgroup v2 `memory.max` limits the effective memory.
#[cfg(target_os = "linux")]
#[test]
fn detects_the_cgroup_v2_limit_of_a_nested_path() {
    let host = FakeHost::new(16 * GIB_KIB, "0::/user.slice/app.slice/run-1.scope\n");
    host.write(
        "sys/fs/cgroup/user.slice/app.slice/run-1.scope/memory.max",
        "1181116006\n",
    );
    host.write("sys/fs/cgroup/memory.max", "max\n");

    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        1_181_116_006
    );
}

/// A finite cgroup v2 limit on an intermediate ancestor applies even when the leaf is unlimited.
#[cfg(target_os = "linux")]
#[test]
fn detects_an_intermediate_cgroup_v2_ancestor_limit() {
    let host = FakeHost::new(16 * GIB_KIB, "0::/docker/parent/leaf\n");
    host.write("sys/fs/cgroup/docker/parent/leaf/memory.max", "max\n");
    host.write("sys/fs/cgroup/docker/parent/memory.max", "3221225472\n");
    host.write("sys/fs/cgroup/memory.max", "8589934592\n");

    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        3 * GIB
    );
}

/// A finite cgroup v1 limit on an intermediate ancestor applies even when the leaf is unlimited.
#[cfg(target_os = "linux")]
#[test]
fn detects_an_intermediate_cgroup_v1_ancestor_limit() {
    let host = FakeHost::new(16 * GIB_KIB, "4:memory:/docker/parent/leaf\n");
    host.write(
        "sys/fs/cgroup/memory/docker/parent/leaf/memory.limit_in_bytes",
        "9223372036854771712\n",
    );
    host.write(
        "sys/fs/cgroup/memory/docker/parent/memory.limit_in_bytes",
        "3221225472\n",
    );
    host.write("sys/fs/cgroup/memory/memory.limit_in_bytes", "8589934592\n");

    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        3 * GIB
    );
}

/// Without a process-specific limit file, the root cgroup's limit applies.
#[cfg(target_os = "linux")]
#[test]
fn falls_back_to_the_root_cgroup_v2_limit() {
    let host = FakeHost::new(16 * GIB_KIB, "0::/missing.scope\n");
    host.write("sys/fs/cgroup/memory.max", "4294967296\n");

    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        4 * GIB
    );
}

/// cgroup v1 reports "unlimited" as a huge sentinel, which leaves the host's memory.
#[cfg(target_os = "linux")]
#[test]
fn ignores_the_cgroup_v1_unlimited_sentinel() {
    let host = FakeHost::new(8 * GIB_KIB, "4:memory:/docker/abcdef\n");
    host.write(
        "sys/fs/cgroup/memory/docker/abcdef/memory.limit_in_bytes",
        "9223372036854771712\n",
    );

    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        8 * GIB
    );
}

/// With both cgroup versions present, the tighter limit applies.
#[cfg(target_os = "linux")]
#[test]
fn chooses_the_tighter_of_both_cgroup_versions() {
    let host = FakeHost::new(16 * GIB_KIB, "4:memory:/docker/abcdef\n0::/docker/abcdef\n");
    host.write("sys/fs/cgroup/docker/abcdef/memory.max", "4294967296\n");
    host.write(
        "sys/fs/cgroup/memory/docker/abcdef/memory.limit_in_bytes",
        "2147483648\n",
    );

    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        2 * GIB
    );
}

/// Without any cgroup limit, and with a limit above the host's memory, the host's memory
/// applies.
#[cfg(target_os = "linux")]
#[test]
fn uses_the_host_memory_without_a_tighter_cgroup_limit() {
    let host = FakeHost::new(8 * GIB_KIB, "0::/\n");
    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        8 * GIB,
        "no limit files"
    );

    host.write("sys/fs/cgroup/memory.max", "68719476736\n");
    assert_eq!(
        host.files()
            .effective_memory_bytes()
            .expect("the fixture is valid"),
        8 * GIB,
        "a limit above the host's memory"
    );
}

/// A `/proc/meminfo` without `MemTotal` is an error, not a zero budget.
#[cfg(target_os = "linux")]
#[test]
fn fails_without_mem_total() {
    let host = FakeHost::new(8 * GIB_KIB, "0::/\n");
    host.write("proc/meminfo", "MemFree: 1024 kB\n");

    let error = host
        .files()
        .effective_memory_bytes()
        .expect_err("the host's memory is unknown");
    assert!(
        error.to_string().contains("MemTotal field missing"),
        "{error}"
    );
}

/// On Linux, detection reads this host's memory.
#[cfg(target_os = "linux")]
#[test]
fn detects_this_hosts_memory() {
    let effective = super::detect().expect("Linux test hosts have /proc/meminfo");

    assert!(effective > 0);
    assert!(effective <= meminfo_total_bytes().expect("Linux test hosts have /proc/meminfo"));
}

/// Elsewhere, detection asks for an explicit budget (D9).
#[cfg(not(target_os = "linux"))]
#[test]
fn asks_for_an_explicit_budget_on_other_platforms() {
    let error = super::detect().expect_err("detection is Linux-only");

    assert!(
        error.to_string().contains("memory_budget to a size"),
        "{error}"
    );
}
