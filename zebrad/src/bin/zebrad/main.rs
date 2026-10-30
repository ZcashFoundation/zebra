//! Main entry point for Zebrad

#[cfg(all(target_os = "linux", target_env = "gnu"))]
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

use zebrad::application::{boot, APPLICATION};

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests;

/// Process entry point for `zebrad`
fn main() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if !has_malloc_arena_override(
        std::env::var_os("MALLOC_ARENA_MAX").as_deref(),
        std::env::var_os("GLIBC_TUNABLES").as_deref(),
    ) {
        // Limit allocator fragmentation before starting threads; preserve operator overrides.
        // SAFETY: mallopt takes integer parameters and M_ARENA_MAX accepts a positive limit.
        #[allow(unsafe_code)]
        unsafe {
            nix::libc::mallopt(nix::libc::M_ARENA_MAX, 16);
        }
    }

    // Enable backtraces by default for zebrad, but allow users to override it.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
        // Disable library backtraces (i.e. eyre) to avoid performance hit for
        // non-panic errors, but allow users to override it.
        if std::env::var_os("RUST_LIB_BACKTRACE").is_none() {
            std::env::set_var("RUST_LIB_BACKTRACE", "0");
        }
    }
    boot(&APPLICATION);
}

/// Returns whether the operator configured a glibc malloc arena limit.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn has_malloc_arena_override(
    malloc_arena_max: Option<&OsStr>,
    glibc_tunables: Option<&OsStr>,
) -> bool {
    malloc_arena_max.is_some()
        || glibc_tunables.is_some_and(|tunables| {
            tunables
                .as_bytes()
                .split(|&byte| byte == b':')
                .any(|entry| entry.starts_with(b"glibc.malloc.arena_max="))
        })
}
