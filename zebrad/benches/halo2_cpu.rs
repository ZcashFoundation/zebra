//! Halo2 batch benchmarks using zebrad's configured global Rayon pool.

#![allow(missing_docs)]

use abscissa_core::Application;
use clap::Parser;

#[path = "../../zebra-consensus/benches/halo2.rs"]
mod halo2;

#[allow(clippy::print_stderr)]
#[cfg(target_os = "linux")]
fn assert_pinned_workers() {
    use std::{collections::HashSet, fs};

    use nix::{
        sched::{sched_getaffinity, CpuSet},
        unistd::Pid,
    };

    let workers = rayon::broadcast(|context| {
        let affinity =
            sched_getaffinity(Pid::from_raw(0)).expect("worker CPU affinity is readable");
        let cpus: Vec<_> = (0..CpuSet::count())
            .filter(|&cpu| affinity.is_set(cpu).expect("CPU index is within CpuSet"))
            .collect();
        assert_eq!(cpus.len(), 1, "each Rayon worker must be pinned to one CPU");
        (context.index(), cpus[0])
    });
    assert_eq!(workers.len(), rayon::current_num_threads());
    let cores: HashSet<_> = workers
        .iter()
        .map(|&(_, cpu)| {
            let root = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            let package = fs::read_to_string(format!("{root}/physical_package_id"))
                .expect("worker's CPU has a package ID");
            let core = fs::read_to_string(format!("{root}/core_id"))
                .expect("worker's CPU has a physical core ID");
            (package, core)
        })
        .collect();
    assert_eq!(
        cores.len(),
        workers.len(),
        "Rayon workers must use different physical cores"
    );
    eprintln!("pinned Rayon worker affinities (worker, CPU): {workers:?}");
}

fn main() {
    // Load the real zebrad TOML config, without relying on the host's default file.
    let config_file = tempfile::NamedTempFile::new().expect("temporary config is writable");
    std::fs::write(
        config_file.path(),
        "[sync]\npin_parallel_cpu_threads = true\n",
    )
    .expect("temporary config is writable");
    let command = zebrad::commands::EntryPoint::parse_from([
        "zebrad".into(),
        "--config".into(),
        config_file.path().as_os_str().to_os_string(),
        "generate".into(),
    ]);
    let mut app = zebrad::application::ZebradApp::default();
    app.init(&command)
        .expect("benchmark config initializes zebrad's global Rayon pool");
    assert!(
        app.config().sync.pin_parallel_cpu_threads,
        "benchmark config enables pinned verification workers"
    );
    #[cfg(target_os = "linux")]
    assert_pinned_workers();
    halo2::run_benchmarks();
}
