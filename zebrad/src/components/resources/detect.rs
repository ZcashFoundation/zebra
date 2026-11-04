//! Detection of the memory available to this process, for `memory_budget = "auto"`.

use color_eyre::{eyre::eyre, Report};

/// Returns the memory available to this process, in bytes: the smaller of the host's total
/// memory and this process's cgroup memory limit.
///
/// # Errors
///
/// If the available memory cannot be detected, for example on a platform without
/// `/proc/meminfo`.
pub fn detect() -> Result<u64, Report> {
    // Integration scaffold: the budget core package moves the `/proc/meminfo` and cgroup parsers
    // here from the zcashd-compat preflight.
    Err(eyre!(
        "could not detect the memory available to Zebra: set [resources] memory_budget to a size, \
         like \"2 GiB\""
    ))
}
