//! Main entry point for Zebrad

use zebrad::application::{boot, APPLICATION};

/// Process entry point for `zebrad`
fn main() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none()
        && !std::env::var("GLIBC_TUNABLES").is_ok_and(|t| t.contains("glibc.malloc.arena_max"))
    {
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
