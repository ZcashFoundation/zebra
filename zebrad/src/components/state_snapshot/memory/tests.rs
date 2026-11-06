//! Snapshot memory derivation, including exact byte boundaries.

use crate::components::resources::{MemoryPlan, PlanInputs, Profile};

use super::*;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// No budget keeps the existing scheduler and host-CPU build parallelism.
#[test]
fn absent_budget_preserves_snapshot_limits() {
    let limits = MemoryLimits::from_plan(&MemoryPlan::default(), 64);

    assert_eq!(limits.scheduler, SchedulerConfig::default());
    assert_eq!(limits.sst_build_parallelism, None);
}

/// The tip pipeline floor splits exactly into one piece reservation and one SST source buffer.
/// The network floor admits two 22 MiB bootstrap connections, but not three.
#[test]
fn floor_plan_prices_downloads_builds_and_bootstrap_peers() {
    let plan = MemoryPlan::clamped_to_floors(GIB, Profile::TipOnly, &PlanInputs::default());
    let limits = MemoryLimits::from_plan(&plan, 64);

    assert_eq!(plan.sync.pipeline_bytes, Some(32 * MIB));
    assert_eq!(limits.scheduler.budget_bytes, 16 * MIB);
    assert_eq!(limits.sst_build_parallelism, Some(1));
    let builds = u64::try_from(limits.sst_build_parallelism.unwrap()).unwrap();
    assert_eq!(limits.scheduler.budget_bytes + builds * 16 * MIB, 32 * MIB);

    let transport = 16 * MIB + 6 * MIB;
    assert_eq!(transport, 23_068_672);
    assert_eq!(limits.scheduler.max_peers, 2);
    assert_eq!(2 * transport, 46_137_344);
    assert_eq!(
        plan.shares
            .get(crate::components::resources::Component::Network),
        57 * MIB
    );
    assert!(2 * transport <= 57 * MIB);
    assert!(3 * transport > 57 * MIB);
}

/// Larger plans divide the whole pipeline between scheduler reservations and full-piece SST
/// buffers, and never derive more build jobs than half the configured CPUs.
#[test]
fn pipeline_and_cpu_limits_both_bound_sst_builds() {
    let plan = MemoryPlan::derive(2 * GIB, Profile::Ibd, &PlanInputs::default())
        .expect("2 GiB covers the IBD floors");
    let pipeline = plan.sync.pipeline_bytes.unwrap();
    let limits = MemoryLimits::from_plan(&plan, 64);
    let builds = u64::try_from(limits.sst_build_parallelism.unwrap()).unwrap();

    assert_eq!(limits.scheduler.budget_bytes, pipeline - pipeline / 2);
    assert_eq!(builds, 10);
    assert!(limits.scheduler.budget_bytes + builds * 16 * MIB <= pipeline);
    assert_eq!(limits.scheduler.max_peers, 6);

    let cpu_limited = MemoryLimits::from_plan(&plan, 4);
    assert_eq!(cpu_limited.sst_build_parallelism, Some(2));
}
