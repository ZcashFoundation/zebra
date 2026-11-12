//! Tests for glibc malloc arena override detection.

use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

use super::has_malloc_arena_override;

#[test]
fn malloc_arena_max_overrides_default() {
    for value in [OsStr::new("32"), OsStr::new(""), OsStr::from_bytes(b"\xff")] {
        assert!(has_malloc_arena_override(Some(value), None));
    }
}

#[test]
fn glibc_tunables_override_default() {
    for value in [
        "glibc.malloc.arena_max=32",
        "glibc.malloc.trim_threshold=128:glibc.malloc.arena_max=32:glibc.malloc.top_pad=64",
    ] {
        assert!(has_malloc_arena_override(None, Some(OsStr::new(value))));
    }
}

#[test]
fn non_utf8_tunable_preserves_arena_override() {
    assert!(has_malloc_arena_override(
        None,
        Some(OsStr::from_bytes(b"ignored=\xff:glibc.malloc.arena_max=32")),
    ));
}

#[test]
fn unrelated_tunable_does_not_override_default() {
    for value in [
        "ignored=glibc.malloc.arena_max",
        "ignored=glibc.malloc.arena_max=32",
        "glibc.malloc.arena_max_extra=32",
        "glibc.malloc.arena_max",
    ] {
        assert!(!has_malloc_arena_override(None, Some(OsStr::new(value))));
    }
}

#[test]
fn absent_variables_do_not_override_default() {
    assert!(!has_malloc_arena_override(None, None));
}
