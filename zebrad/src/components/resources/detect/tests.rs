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
