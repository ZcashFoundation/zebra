//! The memory watchdog: samples the process's memory every 10 seconds, exports the memory
//! metrics, publishes the memory pressure, returns freed heap to the OS when the pressure rises,
//! and switches the plan to the tip-only profile when the node reaches the last checkpoint
//! (design §5, §4.7, §2.3 step 3).
//!
//! The watchdog is advisory: the cgroup or container memory limit is the enforcer.

use std::{
    future::Future,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use color_eyre::Report;
use tokio::sync::watch;

use zebra_chain::{block::Height, chain_tip::ChainTip};

use super::{BudgetSettings, Component, MemoryPlan, MemoryPlanSender};

#[cfg(test)]
mod tests;

/// How often the watchdog samples the process's memory.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// The shortest time between two `malloc_trim` calls (design §4.7, P14).
pub const MIN_TRIM_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The longest time the watchdog waits for a sample or a RocksDB resize, which run on blocking
/// threads, before it skips them and warns.
pub const BLOCKING_TIMEOUT: Duration = Duration::from_secs(60);

/// The percentage of the budget at and above which the pressure is high.
pub const HIGH_PRESSURE_PERCENT: u64 = 85;

/// The percentage of the budget at and above which the pressure is critical.
pub const CRITICAL_PRESSURE_PERCENT: u64 = 95;

/// The process's anonymous memory against its budget.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum MemoryPressure {
    /// Below [`HIGH_PRESSURE_PERCENT`] of the budget, or no budget.
    #[default]
    Normal,

    /// At or above [`HIGH_PRESSURE_PERCENT`] of the budget.
    High,

    /// At or above [`CRITICAL_PRESSURE_PERCENT`] of the budget.
    Critical,
}

impl MemoryPressure {
    /// Returns the pressure of `used` bytes against a `budget`; always `Normal` without one.
    pub fn from_usage(used: u64, budget: Option<u64>) -> Self {
        let Some(budget) = budget else {
            return Self::Normal;
        };

        // Compare percentages without overflow or division: `used × 100 ≥ budget × percent`.
        let used = u128::from(used) * 100;
        let at_least = |percent: u64| used >= u128::from(budget) * u128::from(percent);

        if at_least(CRITICAL_PRESSURE_PERCENT) {
            Self::Critical
        } else if at_least(HIGH_PRESSURE_PERCENT) {
            Self::High
        } else {
            Self::Normal
        }
    }

    /// The value of the `zebra.memory.pressure` metric: 0 normal, 1 high, 2 critical.
    pub fn metric_value(self) -> f64 {
        match self {
            Self::Normal => 0.0,
            Self::High => 1.0,
            Self::Critical => 2.0,
        }
    }
}

/// The process's memory, from `/proc/self/status`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProcessMemory {
    /// The resident set size, `VmRSS`, in bytes.
    pub rss_bytes: u64,

    /// The anonymous part of the resident set, `RssAnon`, in bytes: the memory the budget
    /// shapes, without the reclaimable file-backed pages (design §1.2).
    pub rss_anon_bytes: u64,

    /// The resident set's high-water mark, `VmHWM`, in bytes.
    pub vm_hwm_bytes: u64,
}

impl ProcessMemory {
    /// Parses the contents of a `/proc/<pid>/status` file, or returns `None` if it has no
    /// `VmRSS` line.
    ///
    /// A missing `RssAnon` (kernels before 4.5) is taken to be the whole `VmRSS`, and a missing
    /// `VmHWM` to be `VmRSS`.
    pub fn parse_status(status: &str) -> Option<Self> {
        let field = |name: &str| {
            status.lines().find_map(|line| {
                let value = line.strip_prefix(name)?.strip_prefix(':')?;
                let mut words = value.split_whitespace();
                let kib: u64 = words.next()?.parse().ok()?;
                (words.next() == Some("kB")).then(|| kib.saturating_mul(1024))
            })
        };

        let rss_bytes = field("VmRSS")?;

        Some(Self {
            rss_bytes,
            rss_anon_bytes: field("RssAnon").unwrap_or(rss_bytes),
            vm_hwm_bytes: field("VmHWM").unwrap_or(rss_bytes),
        })
    }

    /// Reads this process's memory, or returns `None` where `/proc/self/status` is missing.
    pub fn read() -> Option<Self> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;

        Self::parse_status(&status)
    }
}

/// The state memory read by one watchdog sample on its blocking worker.
#[derive(Clone, Copy, Debug, Default)]
pub struct StateMemorySample {
    /// RocksDB's memory use and write-stall properties.
    pub rocksdb: zebra_state::RocksDbMemorySample,

    /// The retained charge of distinct blocks in the latest non-finalized state.
    pub non_finalized_resident_bytes: u64,
}

/// Returns the memory a RocksDB sample says the database uses, in bytes: its memtables, block
/// cache and table readers (design §4.1 "Soft bound").
pub fn rocksdb_usage_bytes(sample: &zebra_state::RocksDbMemorySample) -> u64 {
    let memtables = sample
        .write_buffer_manager
        .map_or(0, |manager| manager.usage_bytes);
    let block_cache = sample.block_cache.map_or(0, |cache| cache.usage_bytes);

    memtables
        .saturating_add(block_cache)
        .saturating_add(sample.table_readers_bytes)
}

/// One sample of the node's memory and chain tip.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// When the sample was taken.
    pub now: Instant,

    /// The process's memory, if it could be read.
    pub process: Option<ProcessMemory>,

    /// The RocksDB database's memory use, in bytes, if it could be read.
    pub rocksdb_bytes: Option<u64>,

    /// The height of the state's best tip.
    pub tip: Option<Height>,
}

/// What the watchdog does after a sample.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Actions {
    /// The pressure before and after the sample, when it changed.
    pub pressure_change: Option<(MemoryPressure, MemoryPressure)>,

    /// Whether to return freed heap memory to the OS.
    pub trim: bool,

    /// `Some(true)` when RocksDB's memory crossed above its share of the plan, `Some(false)`
    /// when it went back below.
    pub rocksdb_over_plan: Option<bool>,

    /// `Some(true)` when the sync pipeline's charge crossed above its share of the plan,
    /// `Some(false)` when it went back below.
    pub pipeline_over_plan: Option<bool>,

    /// The new plan, when the profile changed: its RocksDB limits are applied to the open
    /// database.
    pub new_plan: Option<MemoryPlan>,
}

/// The watchdog's state between samples.
#[derive(Debug)]
pub struct Watchdog {
    /// The budget's settings, or `None` without a budget.
    settings: Option<BudgetSettings>,

    /// The plan, which the watchdog re-publishes when the profile changes.
    plan_sender: MemoryPlanSender,

    /// The height of the last checkpoint, where initial sync ends.
    last_checkpoint: Height,

    /// The pipeline whose ceiling changes with the plan.
    pipeline_budget: crate::components::sync::PipelineBudget,

    /// The checkpoint sync engine's lookahead charge, which replaces the legacy syncer's
    /// downloads while the engine runs, so it counts toward the sync pipeline's usage.
    engine_charged: crate::components::checkpoint_sync::engine::Charged,

    /// The pressure of the last sample.
    pressure: MemoryPressure,

    /// When the watchdog last returned freed heap memory to the OS.
    last_trim: Option<Instant>,

    /// Whether RocksDB's memory was above its share of the plan at the last sample.
    rocksdb_over_plan: bool,

    /// Whether the sync pipeline's charge was above its share of the plan at the last sample.
    pipeline_over_plan: bool,
}

impl Watchdog {
    /// Returns a watchdog that re-publishes `plan_sender`'s plan as the tip-only profile once
    /// the tip reaches `last_checkpoint`.
    pub fn new(
        settings: Option<BudgetSettings>,
        plan_sender: MemoryPlanSender,
        last_checkpoint: Height,
    ) -> Self {
        Self {
            settings,
            plan_sender,
            last_checkpoint,
            pipeline_budget: crate::components::sync::PipelineBudget::default(),
            engine_charged: Default::default(),
            pressure: MemoryPressure::Normal,
            last_trim: None,
            rocksdb_over_plan: false,
            pipeline_over_plan: false,
        }
    }

    /// Uses `pipeline_budget` for plan-driven pipeline ceiling updates.
    pub fn with_pipeline_budget(
        mut self,
        pipeline_budget: crate::components::sync::PipelineBudget,
    ) -> Self {
        self.pipeline_budget = pipeline_budget;
        self
    }

    /// Counts the checkpoint sync engine's lookahead charge, `engine_charged`, toward the sync
    /// pipeline's usage.
    pub fn with_engine_charged(
        mut self,
        engine_charged: crate::components::checkpoint_sync::engine::Charged,
    ) -> Self {
        self.engine_charged = engine_charged;
        self
    }

    /// Returns the sync pipeline's usage, in bytes: the retained bytes of the legacy syncer's
    /// downloaded blocks, and the checkpoint sync engine's lookahead charge.
    pub fn pipeline_usage_bytes(&self) -> u64 {
        self.pipeline_budget
            .charged_bytes()
            .saturating_add(self.engine_charged.load(Ordering::Relaxed))
    }

    /// Returns the plan the watchdog publishes.
    pub fn plan(&self) -> MemoryPlan {
        self.plan_sender.borrow().clone()
    }

    /// Returns the pressure of the last sample.
    pub fn pressure(&self) -> MemoryPressure {
        self.pressure
    }

    /// Updates the watchdog's state from `sample`, publishes a new plan when the profile
    /// changes, and returns what to do.
    pub fn observe(&mut self, sample: Sample) -> Actions {
        let mut actions = Actions::default();

        if let Some(new_plan) = self.switch_profile(sample.tip) {
            actions.new_plan = Some(new_plan);
        }
        let plan = self.plan();

        if let Some(process) = sample.process {
            let pressure = MemoryPressure::from_usage(process.rss_anon_bytes, plan.budget);
            if pressure != self.pressure {
                actions.pressure_change = Some((self.pressure, pressure));

                // Trim on the rising edge out of normal pressure, at most once per interval
                // (design §4.7, P14).
                let trim_due = self.last_trim.is_none_or(|last| {
                    sample.now.saturating_duration_since(last) >= MIN_TRIM_INTERVAL
                });
                if self.pressure == MemoryPressure::Normal && trim_due {
                    actions.trim = true;
                    self.last_trim = Some(sample.now);
                }
                self.pressure = pressure;
            }
        }

        if let (Some(rocksdb_bytes), Some(_)) = (sample.rocksdb_bytes, plan.budget) {
            let over_plan = rocksdb_bytes > plan.shares.get(Component::RocksDb);
            if over_plan != self.rocksdb_over_plan {
                actions.rocksdb_over_plan = Some(over_plan);
                self.rocksdb_over_plan = over_plan;
            }
        }

        if plan.budget.is_some() {
            let over_plan = self.pipeline_usage_bytes() > plan.shares.get(Component::Pipeline);
            if over_plan != self.pipeline_over_plan {
                actions.pipeline_over_plan = Some(over_plan);
                self.pipeline_over_plan = over_plan;
            }
        }

        actions
    }

    /// Re-derives and publishes the plan as `TipOnly` when an `Ibd` node's tip reaches the last
    /// checkpoint, and returns the new plan.
    fn switch_profile(&mut self, tip: Option<Height>) -> Option<MemoryPlan> {
        let settings = self.settings.as_ref()?;
        let profile = self.plan_sender.borrow().profile;
        let (new_profile, _) = settings
            .profile_after_open(profile, tip, self.last_checkpoint)
            .ok()?;
        if new_profile == profile {
            return None;
        }

        // The budget covers the `Ibd` floors, so it covers the lower `TipOnly` floors too;
        // keep the current plan if the derivation fails anyway.
        let (plan, _) = settings.plan(new_profile).ok()?;
        tracing::info!(
            "memory profile changed: {profile} → {new_profile}: the sync pipeline's share goes \
             to the other components"
        );
        self.plan_sender.send_replace(plan.clone());

        Some(plan)
    }
}

/// Exports the plan's gauges: the budget, every component's share, the fixed floor, the slack
/// and the profile (design §5).
pub fn export_plan_metrics(plan: &MemoryPlan) {
    // The casts are metric values: f64 represents every memory size below 8 PiB exactly enough
    // for a gauge.
    metrics::gauge!("zebra.memory.budget_bytes").set(plan.budget.unwrap_or(0) as f64);
    for (component, share) in plan.shares.iter() {
        metrics::gauge!("zebra.memory.plan_bytes", "component" => component.label())
            .set(share as f64);
    }
    metrics::gauge!("zebra.memory.plan_bytes", "component" => "fixed").set(plan.fixed as f64);
    metrics::gauge!("zebra.memory.plan_bytes", "component" => "slack").set(plan.slack as f64);
    metrics::gauge!("zebra.memory.profile").set(plan.profile.metric_value());
}

/// Exports the process's memory gauges (design §5).
pub fn export_process_metrics(process: &ProcessMemory) {
    // The casts are metric values: f64 represents every memory size below 8 PiB exactly enough
    // for a gauge.
    metrics::gauge!("zebra.process.rss_bytes").set(process.rss_bytes as f64);
    metrics::gauge!("zebra.process.rss_anon_bytes").set(process.rss_anon_bytes as f64);
    metrics::gauge!("zebra.process.vm_hwm_bytes").set(process.vm_hwm_bytes as f64);
}

/// Returns heap memory that glibc's allocator has freed but kept to the OS, on a blocking
/// thread.
async fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let trim = tokio::task::spawn_blocking(|| {
            // SAFETY: malloc_trim takes a padding size and only releases free heap memory.
            #[allow(unsafe_code)]
            unsafe {
                nix::libc::malloc_trim(0)
            }
        });

        match tokio::time::timeout(BLOCKING_TIMEOUT, trim).await {
            Ok(Ok(released)) => {
                tracing::debug!(released, "returned freed heap memory to the OS")
            }
            Ok(Err(error)) => tracing::warn!(?error, "malloc_trim task failed"),
            Err(_) => tracing::warn!("malloc_trim took longer than {BLOCKING_TIMEOUT:?}"),
        }
    }
}

/// Runs the watchdog until its task is aborted, sampling every [`SAMPLE_INTERVAL`].
///
/// `memory_sample` reads the state's memory sample on a blocking thread, and
/// `apply_state_limits` applies the new non-finalized target and RocksDB limits when the profile
/// changes.
pub async fn run<Tip, Sampler, Apply, Applied>(
    watchdog: Watchdog,
    pressure_sender: watch::Sender<MemoryPressure>,
    latest_chain_tip: Tip,
    memory_sample: Sampler,
    apply_state_limits: Apply,
) -> Result<(), Report>
where
    Tip: ChainTip,
    Sampler: Fn() -> StateMemorySample + Clone + Send + 'static,
    Apply: Fn(zebra_state::MemoryLimits) -> Applied,
    Applied: Future<Output = ()>,
{
    run_with_timing(
        watchdog,
        pressure_sender,
        latest_chain_tip,
        memory_sample,
        apply_state_limits,
        SAMPLE_INTERVAL,
        BLOCKING_TIMEOUT,
    )
    .await
}

async fn run_with_timing<Tip, Sampler, Apply, Applied>(
    mut watchdog: Watchdog,
    pressure_sender: watch::Sender<MemoryPressure>,
    latest_chain_tip: Tip,
    memory_sample: Sampler,
    apply_state_limits: Apply,
    sample_interval: Duration,
    blocking_timeout: Duration,
) -> Result<(), Report>
where
    Tip: ChainTip,
    Sampler: Fn() -> StateMemorySample + Clone + Send + 'static,
    Apply: Fn(zebra_state::MemoryLimits) -> Applied,
    Applied: Future<Output = ()>,
{
    let mut interval = tokio::time::interval(sample_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pending_sample = None;

    loop {
        interval.tick().await;

        if pending_sample.is_none() {
            let has_budget = watchdog.settings.is_some();
            let memory_sample = memory_sample.clone();
            pending_sample = Some(tokio::task::spawn_blocking(move || {
                let sample = has_budget.then(memory_sample);
                let rocksdb_bytes = sample
                    .as_ref()
                    .map(|sample| rocksdb_usage_bytes(&sample.rocksdb));
                let non_finalized_resident_bytes = sample
                    .as_ref()
                    .map(|sample| sample.non_finalized_resident_bytes);
                (
                    ProcessMemory::read(),
                    rocksdb_bytes,
                    non_finalized_resident_bytes,
                )
            }));
        }

        let sample_result = tokio::time::timeout(
            blocking_timeout,
            pending_sample
                .as_mut()
                .expect("a sample was spawned when none was outstanding"),
        )
        .await;
        let (process, rocksdb_bytes, non_finalized_resident_bytes) = match sample_result {
            Ok(Ok(sample)) => {
                pending_sample = None;
                sample
            }
            Ok(Err(error)) => {
                pending_sample = None;
                tracing::warn!(?error, "memory sample task failed");
                (None, None, None)
            }
            Err(_) => {
                tracing::warn!("memory sample took longer than {blocking_timeout:?}");
                (None, None, None)
            }
        };

        let actions = watchdog.observe(Sample {
            now: Instant::now(),
            process,
            rocksdb_bytes,
            tip: latest_chain_tip.best_tip_height(),
        });

        let plan = watchdog.plan();
        export_plan_metrics(&plan);
        metrics::gauge!("zebra.memory.pressure").set(watchdog.pressure().metric_value());
        if let Some(process) = &process {
            export_process_metrics(process);
        }
        if let Some(rocksdb_bytes) = rocksdb_bytes {
            // The cast is a metric value: f64 represents every memory size below 8 PiB exactly
            // enough for a gauge.
            metrics::gauge!("zebra.memory.usage_bytes", "component" => Component::RocksDb.label())
                .set(rocksdb_bytes as f64);
        }
        let pipeline_bytes = watchdog.pipeline_usage_bytes();
        if plan.budget.is_some() {
            // The cast is a metric value: f64 represents every memory size below 8 PiB exactly
            // enough for a gauge.
            metrics::gauge!("zebra.memory.usage_bytes", "component" => Component::Pipeline.label())
                .set(pipeline_bytes as f64);
        }
        if let Some(non_finalized_resident_bytes) = non_finalized_resident_bytes {
            // The cast is a metric value: f64 represents every memory size below 8 PiB exactly
            // enough for a gauge.
            metrics::gauge!(
                "zebra.memory.usage_bytes",
                "component" => Component::NonFinalized.label(),
            )
            .set(non_finalized_resident_bytes as f64);
        }

        if let Some(new_plan) = actions.new_plan {
            if let Some(pipeline_bytes) = new_plan.sync.pipeline_bytes {
                watchdog.pipeline_budget.update_limit(pipeline_bytes);
            }
            if tokio::time::timeout(blocking_timeout, apply_state_limits(new_plan.state))
                .await
                .is_err()
            {
                tracing::warn!("resizing the state database took longer than {blocking_timeout:?}");
            }
        }

        if let Some((before, after)) = actions.pressure_change {
            let rss_anon = process.map_or(0, |process| process.rss_anon_bytes);
            if after > before {
                tracing::warn!(
                    ?before,
                    ?after,
                    rss_anon_bytes = rss_anon,
                    budget_bytes = ?plan.budget,
                    "memory pressure rose: Zebra's anonymous memory is close to its budget"
                );
            } else {
                tracing::info!(
                    ?before,
                    ?after,
                    rss_anon_bytes = rss_anon,
                    "memory pressure fell"
                );
            }
            pressure_sender.send_replace(after);
        }

        if actions.trim {
            trim_heap().await;
        }

        match actions.rocksdb_over_plan {
            Some(true) => tracing::warn!(
                rocksdb_bytes = ?rocksdb_bytes,
                plan_bytes = plan.shares.get(Component::RocksDb),
                "the state database's memtables, block cache and table readers are above its \
                 share of the memory budget: its bounds are soft, and the slack absorbs the \
                 overshoot"
            ),
            Some(false) => tracing::info!(
                rocksdb_bytes = ?rocksdb_bytes,
                "the state database's memory is back within its share of the memory budget"
            ),
            None => {}
        }

        match actions.pipeline_over_plan {
            Some(true) => tracing::warn!(
                pipeline_bytes,
                plan_bytes = plan.shares.get(Component::Pipeline),
                "the sync pipeline's downloaded blocks, with the checkpoint sync engine's \
                 lookahead, are above its share of the memory budget: the oldest checkpoint range \
                 is always admitted, and the slack absorbs the overshoot"
            ),
            Some(false) => tracing::info!(
                pipeline_bytes,
                "the sync pipeline is back within its share of the memory budget"
            ),
            None => {}
        }
    }
}
