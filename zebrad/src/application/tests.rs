use semver::Version;

use super::with_git_commit;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn malloc_arena_operator_settings_are_preserved() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    use super::default_malloc_arena_max;

    for value in [
        OsString::new(),
        OsString::from("8"),
        OsString::from_vec(vec![0xff]),
    ] {
        assert_eq!(default_malloc_arena_max(Some(&value), None), None);
    }

    for tunables in [
        b"glibc.malloc.arena_max=8".as_slice(),
        b"glibc.malloc.arena_max=",
        b"glibc.malloc.arena_max=0:glibc.malloc.trim_threshold=1024",
        b"glibc.malloc.trim_threshold=1024:glibc.malloc.arena_max=4",
        b":glibc.malloc.arena_max=2:",
        b"other=\xff:glibc.malloc.arena_max=8:other=\xfe",
    ] {
        let tunables = OsString::from_vec(tunables.to_vec());
        assert_eq!(default_malloc_arena_max(None, Some(&tunables)), None);
    }

    for tunables in [
        b"".as_slice(),
        b"glibc.malloc.arena_max",
        b"glibc.malloc.arena_max_extra=8",
        b"other=glibc.malloc.arena_max=8",
        b"other=\xff",
    ] {
        let tunables = OsString::from_vec(tunables.to_vec());
        assert!(default_malloc_arena_max(None, Some(&tunables)).is_some());
    }
}

#[test]
fn with_git_commit_adds_commit_without_build_metadata() {
    let version = with_git_commit(Version::new(7, 0, 0), Some("6d1e414"));

    assert_eq!(version.to_string(), "7.0.0+git.6d1e414");
}

#[test]
fn with_git_commit_keeps_git_describe_metadata() {
    let version = Version::parse("7.0.0-rc.0+12.g6d1e414").expect("hard-coded version is valid");

    assert_eq!(with_git_commit(version.clone(), Some("6d1e414")), version);
}

#[test]
fn with_git_commit_skips_missing_or_invalid_commits() {
    let version = Version::new(7, 0, 0);

    assert_eq!(with_git_commit(version.clone(), None), version);
    assert_eq!(
        with_git_commit(version.clone(), Some("not a commit")),
        version
    );
}
