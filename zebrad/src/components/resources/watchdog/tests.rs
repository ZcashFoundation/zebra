//! Tests for the memory watchdog.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use tokio::sync::watch;

use zebra_chain::{block::Height, chain_tip::mock::MockChainTip};

use crate::components::resources::{
    BudgetSettings, BudgetSource, Component, ExplicitLimits, MemoryPlan, PlanInputs, Profile,
};

use super::{
    rocksdb_usage_bytes, run_with_timing, Actions, MemoryPressure, ProcessMemory, Sample, Watchdog,
    MIN_TRIM_INTERVAL,
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// A `/proc/self/status` excerpt from a running zebrad.
const STATUS: &str = "Name:\tzebrad
Umask:\t0022
State:\tS (sleeping)
VmPeak:\t 9473220 kB
VmSize:\t 9408036 kB
VmHWM:\t 1843200 kB
VmRSS:\t  604160 kB
RssAnon:\t  560128 kB
RssFile:\t   44032 kB
RssShmem:\t       0 kB
Threads:\t66
";

/// The watchdog reads the resident set, its anonymous part and its high-water mark.
#[test]
fn parses_proc_self_status() {
    let _init_guard = zebra_test::init();

    assert_eq!(
        ProcessMemory::parse_status(STATUS),
        Some(ProcessMemory {
            rss_bytes: 604_160 * 1024,
            rss_anon_bytes: 560_128 * 1024,
            vm_hwm_bytes: 1_843_200 * 1024,
        })
    );

    assert_eq!(
        ProcessMemory::parse_status("VmRSS:\t 1024 kB\n"),
        Some(ProcessMemory {
            rss_bytes: 1024 * 1024,
            rss_anon_bytes: 1024 * 1024,
            vm_hwm_bytes: 1024 * 1024,
        }),
        "old kernels have no RssAnon"
    );
    assert_eq!(ProcessMemory::parse_status("Name:\tzebrad\n"), None);
    assert_eq!(
        ProcessMemory::parse_status("VmRSS:\t lots kB\n"),
        None,
        "a malformed value is not a zero"
    );
}

/// This process's memory can be read on Linux.
#[cfg(target_os = "linux")]
#[test]
fn reads_this_process_memory() {
    let _init_guard = zebra_test::init();

    let memory = ProcessMemory::read().expect("Linux has /proc/self/status");
    assert!(memory.rss_bytes > 0);
    assert!(memory.rss_anon_bytes <= memory.rss_bytes);
    assert!(memory.vm_hwm_bytes >= memory.rss_bytes);
}

/// The pressure is high from 85 % of the budget and critical from 95 %, and always normal
/// without a budget.
#[test]
fn pressure_follows_the_budget_thresholds() {
    let _init_guard = zebra_test::init();

    for (used, expected) in [
        (0, MemoryPressure::Normal),
        (849, MemoryPressure::Normal),
        (850, MemoryPressure::High),
        (949, MemoryPressure::High),
        (950, MemoryPressure::Critical),
        (5000, MemoryPressure::Critical),
    ] {
        assert_eq!(
            MemoryPressure::from_usage(used, Some(1000)),
            expected,
            "{used}"
        );
    }

    assert_eq!(
        MemoryPressure::from_usage(u64::MAX, None),
        MemoryPressure::Normal
    );
    assert_eq!(
        MemoryPressure::from_usage(u64::MAX, Some(u64::MAX)),
        MemoryPressure::Critical,
        "no overflow at the extremes"
    );
}

/// Returns the settings of a 2 GiB explicit budget.
fn two_gib_settings() -> BudgetSettings {
    BudgetSettings {
        budget: 2 * GIB,
        source: BudgetSource::Explicit,
        inputs: PlanInputs::default(),
        explicit: ExplicitLimits::default(),
        unsafe_low_specs: false,
    }
}

/// Returns a watchdog over `settings`'s initial plan, and a receiver of its plan.
fn watchdog(
    settings: Option<BudgetSettings>,
    last_checkpoint: Height,
) -> (Watchdog, watch::Receiver<MemoryPlan>) {
    let plan = match &settings {
        Some(settings) => {
            settings
                .plan(settings.initial_profile())
                .expect("the test budget covers its floors")
                .0
        }
        None => MemoryPlan::default(),
    };
    let (plan_sender, plan_receiver) = watch::channel(plan);

    (
        Watchdog::new(settings, plan_sender, last_checkpoint),
        plan_receiver,
    )
}

/// Returns a sample of `rss_anon_bytes` at `now`, below the last checkpoint.
fn sample(now: Instant, rss_anon_bytes: u64) -> Sample {
    Sample {
        now,
        process: Some(ProcessMemory {
            rss_bytes: rss_anon_bytes,
            rss_anon_bytes,
            vm_hwm_bytes: rss_anon_bytes,
        }),
        rocksdb_bytes: None,
        tip: Some(Height(1)),
    }
}

/// `malloc_trim` runs on the rising edge out of normal pressure, at most once every 10 minutes
/// (design §4.7, P14).
#[test]
fn trims_on_the_rising_edge_at_most_every_ten_minutes() {
    let _init_guard = zebra_test::init();
    let (mut watchdog, _plan) = watchdog(Some(two_gib_settings()), Height(1_000));
    let start = Instant::now();
    let high = 2 * GIB * 90 / 100;
    let critical = 2 * GIB * 96 / 100;

    assert_eq!(watchdog.observe(sample(start, GIB)), Actions::default());

    let actions = watchdog.observe(sample(start, high));
    assert_eq!(
        actions.pressure_change,
        Some((MemoryPressure::Normal, MemoryPressure::High))
    );
    assert!(actions.trim, "the first rise trims");

    let actions = watchdog.observe(sample(start, critical));
    assert_eq!(
        actions.pressure_change,
        Some((MemoryPressure::High, MemoryPressure::Critical))
    );
    assert!(!actions.trim, "only the edge out of normal trims");

    watchdog.observe(sample(start, GIB));
    let actions = watchdog.observe(sample(start + Duration::from_secs(60), critical));
    assert!(actions.pressure_change.is_some());
    assert!(
        !actions.trim,
        "a second rise within 10 minutes does not trim"
    );

    watchdog.observe(sample(start + Duration::from_secs(70), GIB));
    let actions = watchdog.observe(sample(start + MIN_TRIM_INTERVAL, high));
    assert!(actions.trim, "a rise 10 minutes after the last trim trims");
}

/// Without a budget the pressure stays normal, so nothing is trimmed and no plan changes.
#[test]
fn without_a_budget_the_watchdog_only_observes() {
    let _init_guard = zebra_test::init();
    let (mut watchdog, plan) = watchdog(None, Height(0));

    let actions = watchdog.observe(Sample {
        now: Instant::now(),
        process: Some(ProcessMemory {
            rss_bytes: 64 * GIB,
            rss_anon_bytes: 64 * GIB,
            vm_hwm_bytes: 64 * GIB,
        }),
        rocksdb_bytes: Some(64 * GIB),
        tip: Some(Height(10)),
    });

    assert_eq!(actions, Actions::default());
    assert_eq!(*plan.borrow(), MemoryPlan::default());
}

/// An initial sync node switches to the tip-only plan when its tip reaches the last checkpoint,
/// once (design §2.3 step 3).
#[test]
fn switches_to_tip_only_at_the_last_checkpoint() {
    let _init_guard = zebra_test::init();
    let (mut watchdog, mut plan) = watchdog(Some(two_gib_settings()), Height(1_000));
    let now = Instant::now();
    assert_eq!(plan.borrow_and_update().profile, Profile::Ibd);

    for tip in [None, Some(Height(999))] {
        let actions = watchdog.observe(Sample {
            tip,
            ..sample(now, GIB)
        });
        assert_eq!(
            actions.new_plan, None,
            "{tip:?} is below the last checkpoint"
        );
    }
    assert!(!plan.has_changed().expect("the watchdog holds the sender"));

    let actions = watchdog.observe(Sample {
        tip: Some(Height(1_000)),
        ..sample(now, GIB)
    });
    let new_plan = actions
        .new_plan
        .expect("the tip reached the last checkpoint");
    assert_eq!(new_plan.profile, Profile::TipOnly);
    assert_eq!(
        new_plan,
        two_gib_settings()
            .plan(Profile::TipOnly)
            .expect("2 GiB covers the tip floors")
            .0
    );
    assert!(
        new_plan.state.rocksdb.block_cache_bytes
            > MemoryPlan::derive(2 * GIB, Profile::Ibd, &PlanInputs::default())
                .expect("2 GiB covers the initial sync floors")
                .state
                .rocksdb
                .block_cache_bytes,
        "the pipeline's share goes to RocksDB"
    );
    assert!(plan.has_changed().expect("the watchdog holds the sender"));
    assert_eq!(*plan.borrow_and_update(), new_plan);

    let actions = watchdog.observe(Sample {
        tip: Some(Height(2_000)),
        ..sample(now, GIB)
    });
    assert_eq!(actions.new_plan, None, "the profile changes once");
}

/// RocksDB's memory above its share of the plan is reported once per crossing (P10).
#[test]
fn reports_rocksdb_crossing_its_share_once() {
    let _init_guard = zebra_test::init();
    let (mut watchdog, plan) = watchdog(Some(two_gib_settings()), Height(1_000));
    let share = plan.borrow().shares.get(Component::RocksDb);
    let now = Instant::now();
    let rocksdb = |rocksdb_bytes| Sample {
        rocksdb_bytes: Some(rocksdb_bytes),
        ..sample(now, GIB)
    };

    assert_eq!(watchdog.observe(rocksdb(share)).rocksdb_over_plan, None);
    assert_eq!(
        watchdog.observe(rocksdb(share + 1)).rocksdb_over_plan,
        Some(true)
    );
    assert_eq!(
        watchdog.observe(rocksdb(share + MIB)).rocksdb_over_plan,
        None
    );
    assert_eq!(
        watchdog.observe(rocksdb(share)).rocksdb_over_plan,
        Some(false)
    );
}

/// RocksDB's usage is its memtables, block cache and table readers.
#[test]
fn rocksdb_usage_sums_memtables_cache_and_table_readers() {
    let _init_guard = zebra_test::init();

    let sample = zebra_state::RocksDbMemorySample {
        table_readers_bytes: 3,
        block_cache: Some(zebra_state::BlockCacheSample {
            usage_bytes: 5,
            ..Default::default()
        }),
        write_buffer_manager: Some(zebra_state::WriteBufferManagerSample {
            usage_bytes: 7,
            total_bytes: 100,
        }),
        ..Default::default()
    };

    assert_eq!(rocksdb_usage_bytes(&sample), 15);
    assert_eq!(
        rocksdb_usage_bytes(&zebra_state::RocksDbMemorySample::default()),
        0
    );
}

/// The watchdog reads an open state database's memory and resizes it through the read state
/// service.
#[tokio::test]
async fn reads_and_resizes_the_state_database() {
    let _init_guard = zebra_test::init();

    let network = zebra_chain::parameters::Network::Mainnet;
    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        zebra_state::init_test_services(&network).await;

    let sampled_state = read_state.clone();
    let sample = tokio::task::spawn_blocking(move || sampled_state.memory_sample())
        .await
        .expect("the sample does not panic");
    assert!(
        sample
            .block_cache
            .is_some_and(|cache| cache.capacity_bytes > 0),
        "an open database has a block cache: {sample:?}"
    );

    let tip_only = two_gib_settings()
        .plan(Profile::TipOnly)
        .expect("2 GiB covers the tip floors")
        .0;
    read_state
        .spawn_apply_memory_limits(tip_only.state.rocksdb)
        .await
        .expect("the limits a plan derives apply to an open database");
}

/// A timed-out blocking sample stays outstanding across later deadlines instead of detaching and
/// accumulating more blocking work. Other blocking work and watchdog shutdown remain bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_sample_is_reused_across_timeouts() {
    let _init_guard = zebra_test::init();
    let (watchdog, _plan) = watchdog(Some(two_gib_settings()), Height(1_000));
    let (pressure_sender, _pressure_receiver) = watch::channel(MemoryPressure::Normal);
    let (tip, _tip_sender) = MockChainTip::new();
    let starts = Arc::new(AtomicUsize::new(0));
    let finishes = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));

    let sampler = {
        let starts = starts.clone();
        let finishes = finishes.clone();
        let release = release.clone();
        move || {
            starts.fetch_add(1, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                std::thread::park_timeout(Duration::from_millis(1));
            }
            finishes.fetch_add(1, Ordering::SeqCst);
            zebra_state::RocksDbMemorySample::default()
        }
    };

    let watchdog_task = tokio::spawn(run_with_timing(
        watchdog,
        pressure_sender,
        tip,
        sampler,
        |_| async {},
        Duration::from_millis(5),
        Duration::from_millis(15),
    ));

    let sampler_started = tokio::time::timeout(Duration::from_secs(1), async {
        while starts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(90)).await;
    let observed_starts = starts.load(Ordering::SeqCst);

    let unrelated_ran = Arc::new(AtomicBool::new(false));
    let unrelated_result = {
        let unrelated_ran = unrelated_ran.clone();
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || {
                unrelated_ran.store(true, Ordering::SeqCst);
            }),
        )
        .await
    };

    watchdog_task.abort();
    let shutdown_result = tokio::time::timeout(Duration::from_secs(1), watchdog_task).await;

    release.store(true, Ordering::SeqCst);
    let samples_drained = tokio::time::timeout(Duration::from_secs(1), async {
        while finishes.load(Ordering::SeqCst) < starts.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;

    assert!(
        sampler_started.is_ok(),
        "the watchdog starts its first sample"
    );
    assert_eq!(
        observed_starts, 1,
        "later deadlines must await the outstanding sample instead of spawning another"
    );
    assert!(
        matches!(unrelated_result, Ok(Ok(()))) && unrelated_ran.load(Ordering::SeqCst),
        "unrelated blocking work still runs while the sampler is blocked"
    );
    assert!(
        matches!(shutdown_result, Ok(Err(error)) if error.is_cancelled()),
        "aborting the watchdog does not wait for its blocked sampler"
    );
    assert!(samples_drained.is_ok(), "the released sampler finishes");
}
