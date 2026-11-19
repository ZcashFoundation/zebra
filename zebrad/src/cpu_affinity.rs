//! Linux CPU topology and worker affinity helpers.

#[cfg(target_os = "linux")]
use std::{collections::HashSet, fs, path::Path};

#[cfg(target_os = "linux")]
use nix::{
    sched::{sched_getaffinity, sched_setaffinity, CpuSet},
    unistd::Pid,
};

/// Returns one allowed logical CPU per physical package/core.
///
/// Returns `None` when topology or affinity information is unavailable.
#[cfg(target_os = "linux")]
pub(crate) fn physical_cores() -> Option<Vec<usize>> {
    physical_cores_from(Path::new("/sys/devices/system/cpu"), &allowed_cpus()?)
}

/// Returns `None` when physical CPU topology is unavailable on this platform.
#[cfg(not(target_os = "linux"))]
pub(crate) fn physical_cores() -> Option<Vec<usize>> {
    None
}

#[cfg(target_os = "linux")]
fn physical_cores_from(root: &Path, allowed: &[usize]) -> Option<Vec<usize>> {
    let mut cores = HashSet::new();
    let mut selected = Vec::new();

    for &cpu in allowed {
        let topology = root.join(format!("cpu{cpu}/topology"));
        let package = fs::read_to_string(topology.join("physical_package_id"))
            .ok()?
            .trim()
            .parse::<i32>()
            .ok()?;
        let core = fs::read_to_string(topology.join("core_id"))
            .ok()?
            .trim()
            .parse::<i32>()
            .ok()?;
        if package < 0 || core < 0 {
            return None;
        }

        if cores.insert((package, core)) {
            selected.push(cpu);
        }
    }

    (!selected.is_empty()).then_some(selected)
}

#[cfg(target_os = "linux")]
fn allowed_cpus() -> Option<Vec<usize>> {
    // ponytail: nix's CpuSet has a fixed size; fall back to unpinned workers
    // if the kernel needs a larger affinity mask.
    let mask = sched_getaffinity(Pid::from_raw(0)).ok()?;
    let cpus = (0..CpuSet::count())
        .filter(|&cpu| {
            mask.is_set(cpu)
                .expect("CPU index is below CpuSet::count()")
        })
        .collect::<Vec<_>>();
    (!cpus.is_empty()).then_some(cpus)
}

/// Restricts the calling thread to the specified logical CPU.
///
/// Returns `false` if the affinity setting is unsupported or rejected.
#[cfg(target_os = "linux")]
pub(crate) fn pin_current_thread(cpu: usize) -> bool {
    let mut mask = CpuSet::new();
    mask.set(cpu).is_ok() && sched_setaffinity(Pid::from_raw(0), &mask).is_ok()
}

/// Returns `false` when thread affinity is unavailable on this platform.
#[cfg(not(target_os = "linux"))]
pub(crate) fn pin_current_thread(_cpu: usize) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
