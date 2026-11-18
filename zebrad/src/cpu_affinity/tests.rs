use std::{fs, path::Path};

use super::{allowed_cpus, physical_cores_from, pin_current_thread};

fn core(root: &Path, cpu: usize, package: &str, id: &str) {
    let topology = root.join(format!("cpu{cpu}/topology"));
    fs::create_dir_all(&topology).expect("temporary topology directory is writable");
    fs::write(topology.join("physical_package_id"), package)
        .expect("temporary topology directory is writable");
    fs::write(topology.join("core_id"), id).expect("temporary topology directory is writable");
}

#[test]
fn selects_one_logical_cpu_per_package_and_core() {
    let dir = tempfile::tempdir().expect("temporary topology directory is available");
    core(dir.path(), 0, "0\n", "0\n");
    core(dir.path(), 1, "0\n", "0\n");
    core(dir.path(), 2, "0\n", "1\n");
    core(dir.path(), 3, "1\n", "0\n");

    assert_eq!(
        physical_cores_from(dir.path(), &[0, 1, 2, 3]),
        Some(vec![0, 2, 3])
    );
}

#[test]
fn only_selects_allowed_cpus_even_when_the_first_sibling_is_disallowed() {
    let dir = tempfile::tempdir().expect("temporary topology directory is available");
    // CPUs 0 and 2 are outside the process's allowed CPU set and have no topology fixtures.
    core(dir.path(), 1, "0", "0");
    core(dir.path(), 3, "0", "0");
    core(dir.path(), 4, "0", "1");

    assert_eq!(
        physical_cores_from(dir.path(), &[1, 3, 4]),
        Some(vec![1, 4])
    );
}

#[test]
fn unavailable_or_inconsistent_topology_falls_back() {
    let dir = tempfile::tempdir().expect("temporary topology directory is available");
    assert_eq!(physical_cores_from(dir.path(), &[]), None);
    assert_eq!(physical_cores_from(dir.path(), &[0]), None);

    core(dir.path(), 0, "0", "0");
    fs::remove_file(dir.path().join("cpu0/topology/core_id"))
        .expect("temporary topology directory is writable");
    assert_eq!(physical_cores_from(dir.path(), &[0]), None);
    core(dir.path(), 0, "0", "0");

    core(dir.path(), 1, "-1", "0");
    assert_eq!(physical_cores_from(dir.path(), &[0, 1]), None);

    core(dir.path(), 1, "0", "not a core ID");
    assert_eq!(physical_cores_from(dir.path(), &[0, 1]), None);
}

#[test]
fn pins_only_the_current_thread() {
    let before = allowed_cpus().expect("the Linux thread has allowed CPUs");
    let cpu = before[0];
    std::thread::spawn(move || {
        assert!(pin_current_thread(cpu));
        assert_eq!(allowed_cpus(), Some(vec![cpu]));
        assert!(!pin_current_thread(usize::MAX));
    })
    .join()
    .expect("the affinity test thread completes");
    assert_eq!(allowed_cpus(), Some(before));
}
