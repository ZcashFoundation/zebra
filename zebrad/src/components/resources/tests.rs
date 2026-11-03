//! Tests for resolving the memory budget of a config into a plan.

use color_eyre::eyre::eyre;

use zebra_chain::block::Height;

use crate::config::ZebradConfig;

use super::{
    apply_arena_cap, environment_has_malloc_arena_override, ibd_refusal, init, plan_inputs,
    startup_summary, AllocatorLimits, BudgetSettings, BudgetSource, ExplicitLimits, MemoryBudget,
    MemoryPlan, MinimumBudgets, PlanInputs, Profile,
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Returns a default config with `budget`.
fn config_with_budget(budget: MemoryBudget) -> ZebradConfig {
    let mut config = ZebradConfig::default();
    config.resources.memory_budget = Some(budget);
    config
}

/// Returns the settings and warnings of `config`, with `effective` bytes of detected memory.
fn resolve(
    config: &ZebradConfig,
    unsafe_low_specs: bool,
    effective: Option<u64>,
) -> color_eyre::Result<(BudgetSettings, Vec<String>)> {
    BudgetSettings::from_config(config, unsafe_low_specs, || {
        effective.ok_or_else(|| eyre!("detection fails in this test"))
    })
    .map(|settings| settings.expect("the config has a budget"))
}

/// Without a budget, the plan is today's limits and nothing is detected (D1).
#[test]
fn absent_budget_is_a_strict_no_op() {
    let _init_guard = zebra_test::init();

    let resources = init(&ZebradConfig::default(), false).expect("no budget always starts");
    assert_eq!(resources.settings, None);
    assert_eq!(resources.plan(), MemoryPlan::default());

    let settings = BudgetSettings::from_config(&ZebradConfig::default(), true, || {
        panic!("a node without a budget never detects its memory")
    })
    .expect("no budget always starts");
    assert_eq!(settings, None);
}

/// An explicit budget is used as it is, and opens the profile its floors allow.
#[test]
fn explicit_budget_opens_the_profile_it_covers() {
    let _init_guard = zebra_test::init();

    let (settings, warnings) = resolve(
        &config_with_budget(MemoryBudget::Bytes(2 * GIB)),
        false,
        Some(16 * GIB),
    )
    .expect("2 GiB is a valid budget");
    assert_eq!(settings.budget, 2 * GIB);
    assert_eq!(settings.source, BudgetSource::Explicit);
    assert_eq!(settings.initial_profile(), Profile::Ibd);
    assert!(warnings.is_empty(), "{warnings:?}");

    let (settings, _) = resolve(&config_with_budget(MemoryBudget::Bytes(GIB)), false, None)
        .expect("an explicit budget does not need detection");
    assert_eq!(settings.initial_profile(), Profile::TipOnly);
}

/// `"auto"` budgets half of the detected memory, raises a small host's budget to the initial
/// sync minimum with a warning, and refuses a host below it (design §2.2).
#[test]
fn auto_budget_follows_the_detected_memory() {
    let _init_guard = zebra_test::init();
    let config = config_with_budget(MemoryBudget::Auto);

    let (settings, warnings) = resolve(&config, false, Some(8 * GIB)).expect("8 GiB is plenty");
    assert_eq!(settings.budget, 4 * GIB);
    assert_eq!(settings.source, BudgetSource::Auto { effective: 8 * GIB });
    assert!(warnings.is_empty(), "{warnings:?}");

    let (settings, warnings) = resolve(&config, false, Some(2 * GIB)).expect("2 GiB is enough");
    assert_eq!(
        settings.budget,
        MemoryPlan::min_budget(Profile::Ibd, &settings.inputs)
    );
    assert!(
        warnings[0].starts_with("auto budget raised to the minimum; the host has little headroom"),
        "{warnings:?}"
    );

    let error = resolve(&config, false, Some(GIB)).expect_err("1 GiB cannot sync");
    assert!(
        error
            .to_string()
            .starts_with("memory_budget = \"auto\" needs at least 1,084 MiB of memory for initial sync, but only 1,024 MiB is available to Zebra"),
        "{error}"
    );

    let error = resolve(&config, false, None).expect_err("auto needs detection");
    assert!(error.to_string().contains("detection fails"), "{error}");
}

/// A budget above three quarters of what the host has left after 512 MiB is a warning, and an
/// explicit budget skips the check when detection fails.
#[test]
fn oversized_budget_is_a_warning() {
    let _init_guard = zebra_test::init();
    let config = config_with_budget(MemoryBudget::Bytes(7 * GIB));

    let (_, warnings) = resolve(&config, false, Some(8 * GIB)).expect("7 GiB is a valid budget");
    assert_eq!(
        warnings,
        [
            "memory_budget 7 GiB is more than three quarters of the 8,192 MiB available to Zebra \
          after 512 MiB for the host: the host can run out of memory before Zebra reaches its \
          budget"
        ]
    );

    let (_, warnings) = resolve(&config, false, None).expect("7 GiB is a valid budget");
    assert!(warnings.is_empty(), "{warnings:?}");
}

/// A limit set explicitly in another section wins over the derived one, with a warning when it
/// is above it; an explicit default is treated as unset (design §2.5).
#[test]
fn explicit_limits_win_over_derived_limits() {
    let _init_guard = zebra_test::init();

    // (tx_cost_limit, peerset_initial_target_size, expected plan values, expected warnings)
    for (tx_cost_limit, peers, expected_cost, expected_peers, expected_warnings) in [
        (80_000_000, 25, Some(56_908_316), Some(16), 0),
        (10_000_000, 8, None, None, 0),
        (70_000_000, 20, None, None, 2),
        (80_000_001, 25, None, Some(16), 1),
    ] {
        let mut config = config_with_budget(MemoryBudget::Bytes(2 * GIB));
        config.mempool.tx_cost_limit = tx_cost_limit;
        config.network.peerset_initial_target_size = peers;

        let (settings, _) = resolve(&config, false, None).expect("2 GiB is a valid budget");
        let (plan, warnings) = settings
            .plan(Profile::Ibd)
            .expect("2 GiB covers the floors");

        assert_eq!(plan.mempool.tx_cost_limit, expected_cost, "{tx_cost_limit}");
        assert_eq!(
            plan.network.peerset_initial_target_size, expected_peers,
            "{peers}"
        );
        assert_eq!(warnings.len(), expected_warnings, "{warnings:?}");
    }

    let mut config = config_with_budget(MemoryBudget::Bytes(2 * GIB));
    config.mempool.tx_cost_limit = 70_000_000;
    config.network.peerset_initial_target_size = 20;
    assert_eq!(
        ExplicitLimits::from_config(&config),
        ExplicitLimits {
            tx_cost_limit: Some(70_000_000),
            peerset_initial_target_size: Some(20),
            malloc_arena_max: false,
        }
    );
    let (settings, _) = resolve(&config, false, None).expect("2 GiB is a valid budget");
    let (_, warnings) = settings
        .plan(Profile::Ibd)
        .expect("2 GiB covers the floors");
    assert_eq!(
        warnings,
        [
            "[mempool] tx_cost_limit 70,000,000 is above the 56,908,316 the memory budget \
             derives: using the explicit value, which can exceed the budget",
            "[network] peerset_initial_target_size 20 is above the 16 the memory budget \
             derives: using the explicit value, which can exceed the budget",
        ]
    );
}

/// A budget below the floors is refused with the shortfall, unless `--unsafe-low-specs` runs
/// every component at its floor, with one warning (design §2.3 step 5, D4).
#[test]
fn below_floor_budgets_are_refused_unless_unsafe() {
    let _init_guard = zebra_test::init();
    let config = config_with_budget(MemoryBudget::Bytes(256 * MIB));

    let error = init(&config, false).expect_err("256 MiB is below every floor");
    assert!(
        error
            .to_string()
            .starts_with("memory_budget 256 MiB is below the 933 MiB minimum for a node at the tip (1,084 MiB for initial sync): RocksDB needs 256 MiB"),
        "{error}"
    );

    let (settings, _) = resolve(&config, true, None).expect("an explicit budget parses");
    assert_eq!(settings.initial_profile(), Profile::TipOnly);
    let (plan, warnings) = settings
        .plan(Profile::TipOnly)
        .expect("--unsafe-low-specs runs below the floors");
    assert_eq!(
        plan,
        MemoryPlan::clamped_to_floors(256 * MIB, Profile::TipOnly, &settings.inputs)
    );
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].starts_with("--unsafe-low-specs: memory_budget 256 MiB is below"),
        "{warnings:?}"
    );
}

/// Once the state is open, a tip-only node below the last checkpoint is refused (D15), and an
/// initial sync node at the tip switches to the tip-only profile.
#[test]
fn profile_after_open_follows_the_state_tip() {
    let _init_guard = zebra_test::init();
    let tip_only = config_with_budget(MemoryBudget::Bytes(GIB));
    let (tip_only, _) = resolve(&tip_only, false, None).expect("1 GiB is a valid budget");
    let last_checkpoint = Height(2_000_000);

    let error = tip_only
        .profile_after_open(Profile::TipOnly, Some(Height(10)), last_checkpoint)
        .expect_err("a tip-only budget cannot sync to the last checkpoint");
    assert!(
        error
            .to_string()
            .starts_with("IBD needs >= 1,084 MiB: sync from a snapshot or raise the budget"),
        "{error}"
    );
    let _ = tip_only
        .profile_after_open(Profile::TipOnly, None, last_checkpoint)
        .expect_err("an empty state is below the last checkpoint");

    assert_eq!(
        tip_only
            .profile_after_open(Profile::TipOnly, None, Height(0))
            .expect("a network whose only checkpoint is genesis has nothing to sync"),
        (Profile::TipOnly, None)
    );
    assert_eq!(
        tip_only
            .profile_after_open(Profile::TipOnly, Some(last_checkpoint), last_checkpoint)
            .expect("a node at the last checkpoint is at the tip"),
        (Profile::TipOnly, None)
    );

    let unsafe_tip_only = BudgetSettings {
        unsafe_low_specs: true,
        ..tip_only.clone()
    };
    let (profile, warning) = unsafe_tip_only
        .profile_after_open(Profile::TipOnly, Some(Height(10)), last_checkpoint)
        .expect("--unsafe-low-specs syncs anyway");
    assert_eq!(profile, Profile::TipOnly);
    assert!(
        warning.is_some_and(|warning| warning.contains("IBD needs >= 1,084 MiB")),
        "the override is logged"
    );

    assert_eq!(
        tip_only
            .profile_after_open(Profile::Ibd, Some(last_checkpoint), last_checkpoint)
            .expect("an initial sync node can always continue"),
        (Profile::TipOnly, None)
    );
    assert_eq!(
        tip_only
            .profile_after_open(Profile::Ibd, Some(Height(10)), last_checkpoint)
            .expect("an initial sync node can always continue"),
        (Profile::Ibd, None)
    );

    assert_eq!(
        ibd_refusal(&MinimumBudgets::new(&PlanInputs::default())),
        "IBD needs >= 1,084 MiB: sync from a snapshot or raise the budget"
    );
}

/// The inputs follow the config: the configured CPU threads and whether the node mines.
#[test]
fn plan_inputs_follow_the_config() {
    let _init_guard = zebra_test::init();

    let mut config = ZebradConfig::default();
    config.sync.parallel_cpu_threads = 12;
    config.sync.full_verify_concurrency_limit = 30;
    let inputs = plan_inputs(&config);
    assert_eq!(inputs.cores, 12);
    assert_eq!(inputs.full_verify_concurrency_limit, 30);
    assert!(!inputs.mining);

    config.mining.internal_miner = true;
    assert!(plan_inputs(&config).mining);
}

/// The startup line names the budget, the profile and every component's share and settings
/// (design §5).
#[test]
fn startup_summary_lists_every_component() {
    let _init_guard = zebra_test::init();

    let mut config = config_with_budget(MemoryBudget::Bytes(2 * GIB));
    config.sync.parallel_cpu_threads = 32;
    let (settings, _) = resolve(&config, false, None).expect("2 GiB is a valid budget");
    let (plan, _) = settings
        .plan(Profile::Ibd)
        .expect("2 GiB covers the floors");

    assert_eq!(
        startup_summary(&plan, &settings, &config),
        "memory budget 2 GiB (explicit) profile=ibd fixed=384 MiB slack=307 MiB \
         rocksdb=474 MiB (pinned_metadata 160, write_buffers 188, per_cf 23, block_cache 125, \
         charged_cfs 4) pipeline=339 MiB (download_concurrency 50, queued_blocks 320 MiB) \
         non_finalized=203 MiB (resident target 101 MiB, index_clone_factor 2, not enforced) \
         network=135 MiB (peers 16, quic 2/4/3 MiB) mempool=56,908,316 cost \
         rpc=67 MiB proposal cache allocator arenas=8 (32 cores)"
    );

    let auto = BudgetSettings {
        source: BudgetSource::Auto {
            effective: 8 * GIB - 400 * MIB,
        },
        ..settings
    };
    assert!(startup_summary(&plan, &auto, &config)
        .starts_with("memory budget 2 GiB (auto: effective RAM 7.6 GiB) profile=ibd"));
}

/// The operator's arena cap, from either environment variable, wins over the derived one.
#[cfg(unix)]
#[test]
fn operator_arena_caps_are_detected() {
    use std::ffi::OsStr;

    use super::has_malloc_arena_override;

    let _init_guard = zebra_test::init();

    assert!(has_malloc_arena_override(Some(OsStr::new("32")), None));
    assert!(has_malloc_arena_override(Some(OsStr::new("")), None));
    for tunables in [
        "glibc.malloc.arena_max=32",
        "glibc.malloc.trim_threshold=128:glibc.malloc.arena_max=32",
    ] {
        assert!(
            has_malloc_arena_override(None, Some(OsStr::new(tunables))),
            "{tunables}"
        );
    }
    for tunables in [
        "glibc.malloc.arena_max",
        "glibc.malloc.arena_max_extra=32",
        "ignored=glibc.malloc.arena_max=32",
    ] {
        assert!(
            !has_malloc_arena_override(None, Some(OsStr::new(tunables))),
            "{tunables}"
        );
    }
    assert!(!has_malloc_arena_override(None, None));
}

/// A budget sets the derived arena cap, unless the operator set one; without a budget the cap
/// is left alone (design §4.7).
#[test]
fn arena_cap_needs_a_budget_and_no_operator_cap() {
    let _init_guard = zebra_test::init();

    assert_eq!(apply_arena_cap(&ZebradConfig::default()), None);

    let mut config = config_with_budget(MemoryBudget::Bytes(2 * GIB));
    config.sync.parallel_cpu_threads = 32;
    let expected = (cfg!(all(target_os = "linux", target_env = "gnu"))
        && !environment_has_malloc_arena_override())
    .then_some(AllocatorLimits::arena_max_for_cores(32));
    assert_eq!(apply_arena_cap(&config), expected);

    let (mut settings, _) = resolve(&config, false, None).expect("2 GiB is a valid budget");
    settings.explicit.malloc_arena_max = true;
    let (plan, _) = settings
        .plan(Profile::Ibd)
        .expect("2 GiB covers the floors");
    assert_eq!(plan.allocator.arena_max, None, "the operator's cap wins");
}
