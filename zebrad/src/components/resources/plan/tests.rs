//! Tests for the memory plan derivation.

use std::fmt::Write as _;

use proptest::prelude::*;

use crate::components::{mempool::MempoolMemoryLimits, sync::SyncMemoryLimits};

use super::{
    mib_ceil, mib_floor, AllocatorLimits, AutoBudget, Component, ComponentShares, MemoryPlan,
    PlanInputs, Profile, FIXED_FLOOR_BYTES, GIB, MAX_AUTO_BUDGET_BYTES, MIB,
};

/// Without a budget every component keeps today's limits: the plan is a strict no-op.
#[test]
fn memory_plan_without_a_budget_keeps_todays_limits() {
    let _init_guard = zebra_test::init();

    assert_eq!(
        MemoryPlan::default(),
        MemoryPlan {
            budget: None,
            profile: Profile::Ibd,
            fixed: 0,
            slack: 0,
            shares: ComponentShares::default(),
            state: zebra_state::MemoryLimits::default(),
            network: zebra_network::MemoryLimits::default(),
            sync: SyncMemoryLimits {
                pipeline_bytes: None
            },
            mempool: MempoolMemoryLimits {
                tx_cost_limit: None
            },
            proposal_cache_bytes: 8 * 72 * 1024 * 1024,
            allocator: AllocatorLimits { arena_max: None },
        }
    );
    assert_eq!(
        MemoryPlan::default().network.quic,
        zebra_network::QuicWindows::default()
    );
}

/// The smallest budgets are derived from the floors, and are the design's thresholds:
/// 933 MiB at the tip and 1,084 MiB for initial sync (design §2.3).
#[test]
fn min_budgets_are_the_smallest_budgets_that_cover_the_floors() {
    let _init_guard = zebra_test::init();
    let inputs = PlanInputs::default();

    for (profile, expected) in [(Profile::TipOnly, "933 MiB"), (Profile::Ibd, "1,084 MiB")] {
        let minimum = MemoryPlan::min_budget(profile, &inputs);
        assert_eq!(mib_ceil(minimum), expected, "{profile:?}");

        MemoryPlan::derive(minimum, profile, &inputs)
            .unwrap_or_else(|shortfall| panic!("the minimum covers the floors: {shortfall}"));
        let shortfall = MemoryPlan::derive(minimum - 1, profile, &inputs)
            .expect_err("one byte less does not cover the floors");
        assert_eq!(shortfall.needed, minimum);
        assert_eq!(shortfall.available, minimum - 1);

        // A whole number of mebibytes at the displayed minimum is accepted, one less is not.
        let displayed = minimum.div_ceil(MIB) * MIB;
        assert!(MemoryPlan::derive(displayed, profile, &inputs).is_ok());
        assert!(MemoryPlan::derive(displayed - MIB, profile, &inputs).is_err());
    }

    assert_eq!(
        Profile::for_budget(1084 * MIB, &inputs),
        Profile::Ibd,
        "1,084 MiB covers the initial sync floors"
    );
    assert_eq!(Profile::for_budget(1083 * MIB, &inputs), Profile::TipOnly);
}

/// The floors are the design's: RocksDB 256 MiB, the pipeline one checkpoint range (`Ibd`) or
/// 32 MiB (`TipOnly`), the non-finalized state 64 MiB, the network 32 MiB, the mempool
/// 25 MiB, and nothing for the RPC proposal cache.
#[test]
fn floors_and_ceilings_are_the_designs() {
    let _init_guard = zebra_test::init();

    let floors = |profile| {
        Component::ALL
            .map(|component| component.floor(profile) / MIB)
            .to_vec()
    };
    assert_eq!(floors(Profile::Ibd), [256, 160, 64, 32, 25, 0]);
    assert_eq!(floors(Profile::TipOnly), [256, 32, 64, 32, 25, 0]);
    assert_eq!(
        Component::ALL
            .map(|component| component.ceiling() / MIB)
            .to_vec(),
        [4096, 2048, 2048, 512, 200, 512]
    );
}

/// At 1 GiB, RocksDB is raised to its floor and the deficit is taken from the other
/// components' headroom; the derived settings are the design's 1 GiB row (design §2.3).
#[test]
fn one_gib_tip_only_raises_rocksdb_to_its_floor() {
    let _init_guard = zebra_test::init();

    let plan = MemoryPlan::derive(GIB, Profile::TipOnly, &PlanInputs::default())
        .expect("1 GiB covers the tip floors");
    let mib = |component| plan.shares.get(component) / MIB;

    assert_eq!(mib(Component::RocksDb), 256);
    assert_eq!(mib(Component::Pipeline), 32);
    assert_eq!(mib(Component::NonFinalized), 81);
    assert_eq!(mib(Component::Network), 50);
    assert_eq!(mib(Component::Mempool), 47);
    assert_eq!(mib(Component::Rpc), 19);

    let rocksdb = plan.state.rocksdb;
    assert_eq!(rocksdb.write_buffer_total, Some(64 * MIB));
    assert_eq!(rocksdb.block_cache_bytes, Some(32 * MIB));
    assert_eq!(
        rocksdb.charge_index_and_filter_cfs,
        zebra_state::READ_PATH_COLUMN_FAMILIES
    );
    assert_eq!(rocksdb.effective_write_buffer_size(), Some(8 * MIB));
    assert_eq!(plan.network.peerset_initial_target_size, Some(8));
    assert_eq!(plan.network.quic, zebra_network::QuicWindows::compact());
    assert_eq!(plan.state.queued_blocks_bytes, 64 * 1024 * 1024);
    assert_eq!(plan.sync.pipeline_bytes, Some(32 * MIB));
    assert!(
        plan.proposal_cache_bytes < 63 * 1024 * 1024,
        "no full-size proposal at 1 GiB"
    );
}

/// At 2 GiB, every component gets its nominal share (design §2.3).
#[test]
fn two_gib_ibd_gives_every_component_its_nominal_share() {
    let _init_guard = zebra_test::init();

    let plan = MemoryPlan::derive(2 * GIB, Profile::Ibd, &PlanInputs::default())
        .expect("2 GiB covers the initial sync floors");
    let mib = |component| plan.shares.get(component) / MIB;

    assert_eq!(plan.slack / MIB, 307);
    assert_eq!(mib(Component::RocksDb), 474);
    assert_eq!(mib(Component::Pipeline), 339);
    assert_eq!(mib(Component::NonFinalized), 203);
    assert_eq!(mib(Component::Network), 135);
    assert_eq!(mib(Component::Mempool), 135);
    assert_eq!(mib(Component::Rpc), 67);

    assert_eq!(
        plan.state
            .rocksdb
            .write_buffer_total
            .map(|bytes| bytes / MIB),
        Some(188)
    );
    assert_eq!(
        plan.state
            .rocksdb
            .block_cache_bytes
            .map(|bytes| bytes / MIB),
        Some(125)
    );
    assert_eq!(
        plan.state
            .rocksdb
            .effective_write_buffer_size()
            .map(|bytes| bytes / MIB),
        Some(23)
    );
    assert_eq!(plan.network.peerset_initial_target_size, Some(16));
    assert_eq!(plan.state.queued_blocks_bytes, 320 * 1024 * 1024);
    assert_eq!(
        plan.state.non_finalized_resident_bytes,
        Some(plan.shares.get(Component::NonFinalized) / 2),
        "the resident target is the share over the index clone factor"
    );
}

/// Above a ceiling, the surplus goes to RocksDB and the pipeline 60/40 in `Ibd`, and to RocksDB
/// in `TipOnly`; above every ceiling, the rest of the budget stays unused.
#[test]
fn surplus_above_ceilings_goes_to_rocksdb_and_the_pipeline() {
    let _init_guard = zebra_test::init();
    let inputs = PlanInputs::default();

    let plan = MemoryPlan::derive(4 * GIB, Profile::Ibd, &inputs).expect("4 GiB is enough");
    let mib = |component| plan.shares.get(component) / MIB;
    assert_eq!(
        mib(Component::Mempool),
        200,
        "the mempool is at its ceiling"
    );
    assert_eq!(mib(Component::RocksDb), 1150);
    assert_eq!(mib(Component::Pipeline), 818);
    assert_eq!(plan.mempool.tx_cost_limit, Some(80_000_000));
    assert_eq!(plan.network.quic, zebra_network::QuicWindows::default());
    assert_eq!(plan.network.peerset_initial_target_size, Some(25));

    let tip = MemoryPlan::derive(4 * GIB, Profile::TipOnly, &inputs).expect("4 GiB is enough");
    assert_eq!(tip.shares.get(Component::Pipeline), 32 * MIB);
    assert!(
        tip.shares.get(Component::RocksDb) > plan.shares.get(Component::RocksDb),
        "at the tip, the pipeline's share goes to the other components"
    );

    for profile in [Profile::Ibd, Profile::TipOnly] {
        let plan = MemoryPlan::derive(64 * GIB, profile, &inputs).expect("64 GiB is enough");
        for component in Component::ALL {
            assert!(plan.shares.get(component) <= component.ceiling());
        }
        assert_eq!(plan.shares.get(Component::RocksDb), 4 * GIB);
        assert_eq!(plan.mempool.tx_cost_limit, Some(80_000_000));
        assert_eq!(plan.proposal_cache_bytes, 512 * 1024 * 1024);
    }
}

/// A budget below the floors lists every component its share does not cover, and names both
/// minimums and every way out (design §2.3 step 5, D4).
#[test]
fn shortfall_lists_every_component_below_its_floor() {
    let _init_guard = zebra_test::init();

    let shortfall = MemoryPlan::derive(512 * MIB, Profile::TipOnly, &PlanInputs::default())
        .expect_err("512 MiB does not cover the tip floors");

    assert_eq!(
        shortfall
            .per_component
            .iter()
            .map(|shortfall| shortfall.component)
            .collect::<Vec<_>>(),
        [
            Component::RocksDb,
            Component::NonFinalized,
            Component::Network,
            Component::Mempool,
        ],
        "the pipeline gets its floor first, and the RPC cache has no floor"
    );
    assert_eq!(
        shortfall.to_string(),
        "memory_budget 512 MiB is below the 933 MiB minimum for a node at the tip \
         (1,084 MiB for initial sync): RocksDB needs 256 MiB but its share is 8 MiB, \
         the non-finalized state needs 64 MiB but its share is 3 MiB, \
         the network needs 32 MiB but its share is 2 MiB, \
         the mempool needs 25 MiB but its share is 2 MiB; \
         raise the budget, remove it, sync from a snapshot, or pass --unsafe-low-specs"
    );

    let shortfall = MemoryPlan::derive(GIB, Profile::Ibd, &PlanInputs::default())
        .expect_err("1 GiB does not cover the initial sync floors");
    assert!(
        shortfall
            .to_string()
            .starts_with("memory_budget 1 GiB is below the 1,084 MiB minimum for initial sync (933 MiB for a node at the tip): "),
        "{shortfall}"
    );
}

/// `--unsafe-low-specs` runs every component at its floor.
#[test]
fn clamped_plan_gives_every_component_its_floor() {
    let _init_guard = zebra_test::init();

    let plan = MemoryPlan::clamped_to_floors(256 * MIB, Profile::TipOnly, &PlanInputs::default());

    assert_eq!(plan.budget, Some(256 * MIB));
    assert_eq!(plan.shares, ComponentShares::floors(Profile::TipOnly));
    assert_eq!(plan.state.rocksdb.write_buffer_total, Some(64 * MIB));
    assert_eq!(plan.state.rocksdb.block_cache_bytes, Some(32 * MIB));
    assert_eq!(plan.sync.pipeline_bytes, Some(32 * MIB));
    assert_eq!(
        plan.mempool.tx_cost_limit,
        Some(25 * MIB * 2 / 5),
        "the floor holds a 10 million cost mempool"
    );
    assert_eq!(plan.proposal_cache_bytes, 0);
}

/// Mining moves 5 % of the components' memory from the pipeline to the RPC proposal cache.
#[test]
fn mining_moves_pipeline_share_to_the_proposal_cache() {
    let _init_guard = zebra_test::init();

    let inputs = PlanInputs::default();
    let mining = PlanInputs {
        mining: true,
        ..inputs
    };
    let plan = MemoryPlan::derive(2 * GIB, Profile::Ibd, &inputs).expect("2 GiB is enough");
    let mining_plan = MemoryPlan::derive(2 * GIB, Profile::Ibd, &mining).expect("2 GiB is enough");

    let moved = mining_plan.shares.get(Component::Rpc) - plan.shares.get(Component::Rpc);
    assert_eq!(
        plan.shares.get(Component::Pipeline) - mining_plan.shares.get(Component::Pipeline),
        moved,
    );
    assert_eq!(moved / MIB, 67, "5 % of the 1,357 MiB the components share");
}

/// At the tip, the pipeline holds the full-verification lookahead at the measured charge per
/// block, between 32 MiB and a quarter of the components' memory.
#[test]
fn tip_only_pipeline_follows_the_measured_charge() {
    let _init_guard = zebra_test::init();

    let pipeline = |ewma_charge| {
        let inputs = PlanInputs {
            ewma_charge,
            full_verify_concurrency_limit: 20,
            ..PlanInputs::default()
        };
        MemoryPlan::derive(4 * GIB, Profile::TipOnly, &inputs)
            .expect("4 GiB is enough")
            .shares
            .get(Component::Pipeline)
    };

    assert_eq!(pipeline(None), 32 * MIB);
    assert_eq!(
        pipeline(Some(MIB)),
        32 * MIB,
        "20 small blocks fit in the floor"
    );
    assert_eq!(pipeline(Some(4 * MIB)), 80 * MIB);
    assert_eq!(
        pipeline(Some(GIB)),
        MemoryPlan::components_memory(4 * GIB, &PlanInputs::default()) / 4,
        "at most a quarter of the components' memory"
    );
}

/// The arena cap is a quarter of the cores, between 2 and 16 (design §6.1).
#[test]
fn arena_max_is_a_quarter_of_the_cores() {
    let _init_guard = zebra_test::init();

    for (cores, arenas) in [
        (0, 2),
        (1, 2),
        (8, 2),
        (12, 3),
        (32, 8),
        (64, 16),
        (128, 16),
    ] {
        assert_eq!(
            AllocatorLimits::arena_max_for_cores(cores),
            arenas,
            "{cores}"
        );
    }
}

/// `"auto"` budgets half the effective memory, at least the initial sync minimum and at most
/// 8 GiB, and refuses a host below the minimum (design §2.2, C8).
#[test]
fn auto_budget_is_half_the_memory_within_the_clamp() {
    let _init_guard = zebra_test::init();
    let inputs = PlanInputs::default();
    let minimum = 1084 * MIB;

    let shortfall = MemoryPlan::auto_budget(GIB, &inputs)
        .expect_err("a 1 GiB host cannot sync, so auto refuses it");
    assert_eq!(shortfall.profile, Profile::Ibd);
    assert_eq!(shortfall.available, GIB);
    MemoryPlan::auto_budget(minimum - 1, &inputs)
        .expect_err("the minimum rounds up to a whole number of mebibytes");

    assert_eq!(
        MemoryPlan::auto_budget(minimum, &inputs),
        Ok(AutoBudget {
            budget: minimum,
            raised_to_minimum: true
        })
    );
    assert_eq!(
        MemoryPlan::auto_budget(2 * GIB, &inputs),
        Ok(AutoBudget {
            budget: minimum,
            raised_to_minimum: true
        }),
        "half of 2 GiB is below the minimum"
    );
    assert_eq!(
        MemoryPlan::auto_budget(8 * GIB, &inputs),
        Ok(AutoBudget {
            budget: 4 * GIB,
            raised_to_minimum: false
        })
    );
    assert_eq!(
        MemoryPlan::auto_budget(64 * GIB, &inputs),
        Ok(AutoBudget {
            budget: MAX_AUTO_BUDGET_BYTES,
            raised_to_minimum: false
        })
    );
    assert_eq!(
        MemoryPlan::auto_budget(5 * GIB + 3, &inputs),
        Ok(AutoBudget {
            budget: 2560 * MIB,
            raised_to_minimum: false
        }),
        "half of the memory rounds down to a whole number of mebibytes"
    );
}

/// A budget above three quarters of the memory left after 512 MiB for the host is oversized.
#[test]
fn oversized_budgets_leave_the_host_too_little() {
    let _init_guard = zebra_test::init();

    assert!(!MemoryPlan::is_oversized(4 * GIB, 8 * GIB));
    assert!(!MemoryPlan::is_oversized(5760 * MIB, 8 * GIB));
    assert!(MemoryPlan::is_oversized(5760 * MIB + 1, 8 * GIB));
    assert!(MemoryPlan::is_oversized(GIB, 512 * MIB));
}

/// The derived plan of every budget in the design's table, in both profiles, so that a change
/// to a share, floor or ceiling is a reviewed snapshot diff (design §6.1).
#[test]
fn derived_plans_golden_table() {
    let _init_guard = zebra_test::init();

    let mut table = String::new();
    for mining in [false, true] {
        let inputs = PlanInputs {
            cores: 32,
            mining,
            ..PlanInputs::default()
        };
        for profile in [Profile::TipOnly, Profile::Ibd] {
            for budget in [512 * MIB, GIB, 2 * GIB, 4 * GIB, 8 * GIB, 16 * GIB] {
                write!(
                    table,
                    "{profile} mining={mining} budget={}: ",
                    mib_floor(budget)
                )
                .expect("writing to a string succeeds");
                match MemoryPlan::derive(budget, profile, &inputs) {
                    Ok(plan) => writeln!(table, "{}", describe(&plan)),
                    Err(shortfall) => writeln!(table, "refused: {shortfall}"),
                }
                .expect("writing to a string succeeds");
            }
        }
    }

    insta::assert_snapshot!(table);
}

/// Describes every share and derived limit of `plan`, in whole mebibytes.
fn describe(plan: &MemoryPlan) -> String {
    let rocksdb = plan.state.rocksdb;
    let quic = plan.network.quic;

    format!(
        "slack={} rocksdb={} (write_buffers {}, block_cache {}) pipeline={} \
         (queued_blocks {}) non_finalized={} (resident {}) network={} (peers {}, quic {}/{}/{}) \
         mempool={} (cost {}) rpc={} arenas={}",
        plan.slack / MIB,
        plan.shares.get(Component::RocksDb) / MIB,
        rocksdb.write_buffer_total.unwrap_or_default() / MIB,
        rocksdb.block_cache_bytes.unwrap_or_default() / MIB,
        plan.shares.get(Component::Pipeline) / MIB,
        plan.state.queued_blocks_bytes / 1024 / 1024,
        plan.shares.get(Component::NonFinalized) / MIB,
        plan.state.non_finalized_resident_bytes.unwrap_or_default() / MIB,
        plan.shares.get(Component::Network) / MIB,
        plan.network.peerset_initial_target_size.unwrap_or_default(),
        quic.stream_receive_window / 1024 / 1024,
        quic.connection_receive_window / 1024 / 1024,
        quic.send_window / 1024 / 1024,
        plan.shares.get(Component::Mempool) / MIB,
        plan.mempool.tx_cost_limit.unwrap_or_default(),
        plan.shares.get(Component::Rpc) / MIB,
        plan.allocator.arena_max.unwrap_or_default(),
    )
}

/// The derived limits of `plan` that grow with the budget, in one comparable list.
fn monotone_limits(plan: &MemoryPlan) -> Vec<u64> {
    let mut limits: Vec<u64> = plan.shares.iter().map(|(_, share)| share).collect();
    limits.extend([
        plan.state.rocksdb.write_buffer_total.unwrap_or_default(),
        plan.state.rocksdb.block_cache_bytes.unwrap_or_default(),
        plan.state.non_finalized_resident_bytes.unwrap_or_default(),
        u64::try_from(plan.state.queued_blocks_bytes).expect("fits in a u64"),
        u64::try_from(plan.network.peerset_initial_target_size.unwrap_or_default())
            .expect("fits in a u64"),
        u64::from(plan.network.quic.stream_receive_window),
        plan.mempool.tx_cost_limit.unwrap_or_default(),
        u64::try_from(plan.proposal_cache_bytes).expect("fits in a u64"),
    ]);
    limits
}

/// Budgets around the floors, where most of the derivation happens, and up to 64 GiB.
fn any_budget() -> impl Strategy<Value = u64> {
    prop_oneof![0..=2 * GIB, 0..=64 * GIB]
}

fn any_profile() -> impl Strategy<Value = Profile> {
    prop_oneof![Just(Profile::Ibd), Just(Profile::TipOnly)]
}

fn any_inputs() -> impl Strategy<Value = PlanInputs> {
    (
        prop_oneof![Just(160 * MIB), Just(FIXED_FLOOR_BYTES)],
        1..=256_usize,
        1..=4_u64,
        any::<bool>(),
        proptest::option::of(0..=4 * MIB),
    )
        .prop_map(
            |(fixed_floor_bytes, cores, index_clone_factor, mining, ewma_charge)| PlanInputs {
                fixed_floor_bytes,
                cores,
                index_clone_factor,
                mining,
                ewma_charge,
                ..PlanInputs::default()
            },
        )
}

proptest! {
    /// A derived plan never gives the components more than the budget allows, keeps every
    /// component between its floor and its ceiling, and refuses exactly the budgets whose
    /// components' memory is below the floors (design §2.3, §6.1).
    #[test]
    fn derived_plans_fit_the_budget(
        budget in any_budget(),
        profile in any_profile(),
        inputs in any_inputs(),
    ) {
        let _init_guard = zebra_test::init();
        let components_memory = MemoryPlan::components_memory(budget, &inputs);
        let floors = ComponentShares::floors(profile);

        match MemoryPlan::derive(budget, profile, &inputs) {
            Ok(plan) => {
                prop_assert!(components_memory >= floors.total());
                prop_assert!(budget >= MemoryPlan::min_budget(profile, &inputs));
                prop_assert!(
                    plan.shares.total() + plan.fixed + plan.slack <= budget,
                    "{plan:?} overspends {budget}"
                );
                for (component, share) in plan.shares.iter() {
                    prop_assert!(share >= component.floor(profile), "{component:?} {plan:?}");
                    prop_assert!(share <= component.ceiling(), "{component:?} {plan:?}");
                }
                prop_assert_eq!(MemoryPlan::derive(budget, profile, &inputs), Ok(plan));
            }
            Err(shortfall) => {
                prop_assert!(components_memory < floors.total());
                prop_assert!(budget < MemoryPlan::min_budget(profile, &inputs));
                prop_assert!(!shortfall.per_component.is_empty());
                for component in shortfall.per_component {
                    prop_assert!(component.share < component.floor);
                }
            }
        }
    }

    /// Every derived limit is monotone non-decreasing in the budget within a profile.
    #[test]
    fn derived_limits_grow_with_the_budget(
        budgets in (any_budget(), any_budget()),
        profile in any_profile(),
        inputs in any_inputs(),
    ) {
        let _init_guard = zebra_test::init();
        let (smaller, larger) = (budgets.0.min(budgets.1), budgets.0.max(budgets.1));

        if let (Ok(smaller_plan), Ok(larger_plan)) = (
            MemoryPlan::derive(smaller, profile, &inputs),
            MemoryPlan::derive(larger, profile, &inputs),
        ) {
            for (index, (small, large)) in monotone_limits(&smaller_plan)
                .into_iter()
                .zip(monotone_limits(&larger_plan))
                .enumerate()
            {
                prop_assert!(
                    small <= large,
                    "limit {index} shrinks from {small} at {smaller} to {large} at {larger}"
                );
            }
        }
    }

    /// A budget one byte larger never shrinks a limit, which catches rounding that the random
    /// pairs above rarely hit.
    #[test]
    fn derived_limits_grow_byte_by_byte(
        budget in 900 * MIB..=64 * GIB,
        profile in any_profile(),
        inputs in any_inputs(),
    ) {
        let _init_guard = zebra_test::init();

        if let (Ok(smaller_plan), Ok(larger_plan)) = (
            MemoryPlan::derive(budget, profile, &inputs),
            MemoryPlan::derive(budget + 1, profile, &inputs),
        ) {
            for (small, large) in monotone_limits(&smaller_plan)
                .into_iter()
                .zip(monotone_limits(&larger_plan))
            {
                prop_assert!(small <= large);
            }
        }
    }

    /// `"auto"` never derives a budget that is refused, on any host it accepts.
    #[test]
    fn auto_budgets_are_never_refused(
        effective in 0..=256 * GIB,
        inputs in any_inputs(),
    ) {
        let _init_guard = zebra_test::init();

        match MemoryPlan::auto_budget(effective, &inputs) {
            Ok(auto) => {
                prop_assert!(auto.budget <= effective);
                prop_assert_eq!(Profile::for_budget(auto.budget, &inputs), Profile::Ibd);
                prop_assert!(MemoryPlan::derive(auto.budget, Profile::Ibd, &inputs).is_ok());
                prop_assert!(MemoryPlan::derive(auto.budget, Profile::TipOnly, &inputs).is_ok());
                prop_assert_eq!(auto.budget % MIB, 0, "auto budgets are whole mebibytes");
            }
            Err(_) => prop_assert!(
                effective < MemoryPlan::min_budget(Profile::Ibd, &inputs).div_ceil(MIB) * MIB
            ),
        }
    }
}
