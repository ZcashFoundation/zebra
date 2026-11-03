//! The memory budget: one `[resources] memory_budget` setting from which zebrad derives the memory
//! limits of every component.
//!
//! Without a budget, which is the default, every component keeps today's limits.

use std::thread::available_parallelism;

use color_eyre::{eyre::eyre, Report};
use tokio::sync::watch;

use zebra_chain::block::Height;

use crate::{components::mempool, config::ZebradConfig};

pub mod config;
pub mod detect;
pub mod plan;
pub mod watchdog;

#[cfg(test)]
mod tests;

pub use config::{Config, MemoryBudget, ParseMemoryBudgetError};
pub use plan::{
    AllocatorLimits, AutoBudget, Component, ComponentShares, ComponentShortfall, MemoryPlan,
    MemoryPlanReceiver, MemoryPlanSender, MinimumBudgets, PlanInputs, Profile, Shortfall,
};

use plan::{mib_ceil, mib_floor, with_thousands_separators, ROCKSDB_PINNED_METADATA_BYTES};

/// One gibibyte, in bytes.
const GIB: u64 = 1024 * 1024 * 1024;

/// How a node's memory budget was chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetSource {
    /// The operator set `memory_budget` to a size.
    Explicit,

    /// `memory_budget = "auto"` derived the budget from the memory available to Zebra.
    Auto {
        /// The memory available to Zebra, in bytes.
        effective: u64,
    },
}

/// The limits an operator set in other config sections, which win over the limits a budget
/// derives (design §2.5).
///
/// A field is explicit when its value differs from its default: an explicit default cannot be
/// told apart from an absent field, so it is treated as unset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExplicitLimits {
    /// `[mempool] tx_cost_limit`, if it is not its default.
    pub tx_cost_limit: Option<u64>,

    /// `[network] peerset_initial_target_size`, if it is not its default.
    pub peerset_initial_target_size: Option<usize>,
}

impl ExplicitLimits {
    /// Returns the limits `config` sets explicitly.
    pub fn from_config(config: &ZebradConfig) -> Self {
        let tx_cost_limit = config.mempool.tx_cost_limit;
        let peerset_initial_target_size = config.network.peerset_initial_target_size;

        Self {
            tx_cost_limit: (tx_cost_limit != mempool::Config::default().tx_cost_limit)
                .then_some(tx_cost_limit),
            peerset_initial_target_size: (peerset_initial_target_size
                != zebra_network::Config::default().peerset_initial_target_size)
                .then_some(peerset_initial_target_size),
        }
    }
}

/// A node's memory budget, and everything else its plan is derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetSettings {
    /// The budget, in bytes.
    pub budget: u64,

    /// How the budget was chosen.
    pub source: BudgetSource,

    /// The derivation's inputs other than the budget and profile.
    pub inputs: PlanInputs,

    /// The limits the operator set explicitly.
    pub explicit: ExplicitLimits,

    /// Whether `zebrad start --unsafe-low-specs` runs a budget below the floors, and a node at
    /// a tip-only budget below the last checkpoint, anyway.
    pub unsafe_low_specs: bool,
}

impl BudgetSettings {
    /// Returns the budget settings of `config`, or `None` without a budget, and the warnings to
    /// log about the budget.
    ///
    /// `detect` returns the memory available to Zebra: it is needed for an `"auto"` budget, and
    /// used for the oversize warning of an explicit budget when it succeeds.
    ///
    /// # Errors
    ///
    /// If the budget is `"auto"` and the available memory cannot be detected, or is below the
    /// initial sync minimum.
    pub fn from_config(
        config: &ZebradConfig,
        unsafe_low_specs: bool,
        detect: impl FnOnce() -> Result<u64, Report>,
    ) -> Result<Option<(Self, Vec<String>)>, Report> {
        let Some(memory_budget) = config.resources.memory_budget else {
            return Ok(None);
        };

        let inputs = plan_inputs(config);
        let mut warnings = Vec::new();

        let (budget, source, effective) = match memory_budget {
            MemoryBudget::Auto => {
                let effective = detect()?;
                let auto = MemoryPlan::auto_budget(effective, &inputs).map_err(|shortfall| {
                    eyre!(
                        "memory_budget = \"auto\" needs at least {} of memory for initial sync, \
                         but only {} is available to Zebra: set memory_budget to a size of at \
                         least {} for a node at the tip, remove it, or give Zebra more memory",
                        mib_ceil(shortfall.minimums.ibd),
                        mib_floor(effective),
                        mib_ceil(shortfall.minimums.tip_only),
                    )
                })?;
                if auto.raised_to_minimum {
                    warnings.push(format!(
                        "auto budget raised to the minimum; the host has little headroom: \
                         memory_budget is {} of the {} available to Zebra",
                        MemoryBudget::Bytes(auto.budget),
                        mib_floor(effective),
                    ));
                }

                (
                    auto.budget,
                    BudgetSource::Auto { effective },
                    Some(effective),
                )
            }
            MemoryBudget::Bytes(budget) => (budget, BudgetSource::Explicit, detect().ok()),
        };

        if let Some(effective) = effective {
            if MemoryPlan::is_oversized(budget, effective) {
                warnings.push(format!(
                    "memory_budget {} is more than three quarters of the {} available to Zebra \
                     after 512 MiB for the host: the host can run out of memory before Zebra \
                     reaches its budget",
                    MemoryBudget::Bytes(budget),
                    mib_floor(effective),
                ));
            }
        }

        Ok(Some((
            Self {
                budget,
                source,
                inputs,
                explicit: ExplicitLimits::from_config(config),
                unsafe_low_specs,
            },
            warnings,
        )))
    }

    /// Returns the profile a node opens its state with: `Ibd` when the budget covers its
    /// floors, or `TipOnly` (design §2.3 step 3).
    pub fn initial_profile(&self) -> Profile {
        Profile::for_budget(self.budget, &self.inputs)
    }

    /// Returns the smallest budgets of both profiles.
    pub fn minimums(&self) -> MinimumBudgets {
        MinimumBudgets::new(&self.inputs)
    }

    /// Derives the plan for `profile`, and returns it with the warnings to log about it.
    ///
    /// Below the floors, `--unsafe-low-specs` runs every component at its floor with a warning
    /// (design §2.3 step 5, D4). Explicit limits win over derived ones, with a warning when they
    /// are above them (§2.5).
    ///
    /// # Errors
    ///
    /// If the budget is below the floors of `profile`, without `--unsafe-low-specs`.
    pub fn plan(&self, profile: Profile) -> Result<(MemoryPlan, Vec<String>), Report> {
        let mut warnings = Vec::new();

        let mut plan = match MemoryPlan::derive(self.budget, profile, &self.inputs) {
            Ok(plan) => plan,
            Err(shortfall) if self.unsafe_low_specs => {
                warnings.push(format!(
                    "--unsafe-low-specs: {shortfall}. Running every component at its floor \
                     instead, so Zebra uses more memory than its budget"
                ));
                MemoryPlan::clamped_to_floors(self.budget, profile, &self.inputs)
            }
            Err(shortfall) => return Err(eyre!("{shortfall}")),
        };

        if let Some(explicit) = self.explicit.tx_cost_limit {
            if let Some(derived) = plan.mempool.tx_cost_limit.take() {
                if explicit > derived {
                    warnings.push(format!(
                        "[mempool] tx_cost_limit {} is above the {} the memory budget derives: \
                         using the explicit value, which can exceed the budget",
                        with_thousands_separators(explicit),
                        with_thousands_separators(derived),
                    ));
                }
            }
        }

        if let Some(explicit) = self.explicit.peerset_initial_target_size {
            if let Some(derived) = plan.network.peerset_initial_target_size.take() {
                if explicit > derived {
                    warnings.push(format!(
                        "[network] peerset_initial_target_size {explicit} is above the {derived} \
                         the memory budget derives: using the explicit value, which can exceed \
                         the budget"
                    ));
                }
            }
        }

        Ok((plan, warnings))
    }

    /// Returns the profile a node runs with once its state is open, given its state tip and the
    /// last checkpoint (design §2.3 step 3, D15):
    ///
    /// - a `TipOnly` node below the last checkpoint is refused, because initial sync needs the
    ///   `Ibd` floors, unless `--unsafe-low-specs` runs it anyway;
    /// - an `Ibd` node at or above the last checkpoint switches to `TipOnly`;
    /// - otherwise the profile is unchanged.
    ///
    /// A missing tip is treated as the genesis height: a network whose only checkpoint is its
    /// genesis block, like Regtest, has nothing to sync below its last checkpoint.
    ///
    /// # Errors
    ///
    /// If a `TipOnly` node is below the last checkpoint, without `--unsafe-low-specs`.
    pub fn profile_after_open(
        &self,
        profile: Profile,
        tip: Option<Height>,
        last_checkpoint: Height,
    ) -> Result<(Profile, Option<String>), Report> {
        let tip_height = tip.unwrap_or(Height(0));
        let below_last_checkpoint = tip_height < last_checkpoint;

        match (profile, below_last_checkpoint) {
            (Profile::TipOnly, true) => {
                let message = format!(
                    "{} (memory_budget {} is only enough for a node at the tip, and the state \
                     tip at height {} is below the last checkpoint at height {})",
                    ibd_refusal(&self.minimums()),
                    MemoryBudget::Bytes(self.budget),
                    tip_height.0,
                    last_checkpoint.0,
                );
                if self.unsafe_low_specs {
                    Ok((
                        Profile::TipOnly,
                        Some(format!(
                            "--unsafe-low-specs: {message}; syncing anyway, so Zebra uses \
                             more memory than its budget"
                        )),
                    ))
                } else {
                    Err(eyre!(
                        "{message}; or pass --unsafe-low-specs to sync over the budget"
                    ))
                }
            }
            (Profile::Ibd, false) => Ok((Profile::TipOnly, None)),
            (profile, _) => Ok((profile, None)),
        }
    }
}

/// Returns the derivation inputs `config` sets: the cores the node uses, whether it mines, and
/// its full-verification concurrency.
pub fn plan_inputs(config: &ZebradConfig) -> PlanInputs {
    PlanInputs {
        cores: node_cores(config),
        mining: config.mining.miner_address.is_some() || config.mining.internal_miner,
        full_verify_concurrency_limit: config.sync.full_verify_concurrency_limit,
        ..PlanInputs::default()
    }
}

/// Returns the CPU cores the node uses: `[sync] parallel_cpu_threads`, or every logical core.
pub fn node_cores(config: &ZebradConfig) -> usize {
    match config.sync.parallel_cpu_threads {
        0 => available_parallelism().map_or(1, usize::from),
        threads => threads,
    }
}

/// Returns the refusal of a node at a tip-only budget that needs initial sync (D15).
pub fn ibd_refusal(minimums: &MinimumBudgets) -> String {
    format!(
        "IBD needs >= {}: sync from a snapshot or raise the budget",
        mib_ceil(minimums.ibd)
    )
}

/// Returns the one-line summary of `plan` that zebrad logs at startup (design §5).
pub fn startup_summary(
    plan: &MemoryPlan,
    settings: &BudgetSettings,
    config: &ZebradConfig,
) -> String {
    let Some(budget) = plan.budget else {
        return "no memory budget: every component keeps its default limits".to_string();
    };

    let cores = settings.inputs.cores;
    let source = match settings.source {
        BudgetSource::Explicit => "explicit".to_string(),
        BudgetSource::Auto { effective } => {
            // The cast is display-only: f64 represents every memory size below 8 PiB to well
            // within one decimal place of a GiB.
            let effective_gib = effective as f64 / GIB as f64;
            format!("auto: effective RAM {effective_gib:.1} GiB")
        }
    };

    let rocksdb = plan.state.rocksdb;
    let quic = plan.network.quic;
    let peers = match plan.network.peerset_initial_target_size {
        Some(peers) => peers.to_string(),
        None => format!("{} explicit", config.network.peerset_initial_target_size),
    };
    let tx_cost_limit = match plan.mempool.tx_cost_limit {
        Some(cost) => format!("{} cost", with_thousands_separators(cost)),
        None => format!(
            "{} cost explicit",
            with_thousands_separators(config.mempool.tx_cost_limit)
        ),
    };
    let mib = |bytes: u64| bytes / (1024 * 1024);
    let usize_mib = |bytes: usize| bytes / (1024 * 1024);
    let u32_mib = |bytes: u32| bytes / (1024 * 1024);

    format!(
        "memory budget {} ({source}) profile={} fixed={} slack={} \
         rocksdb={} (pinned_metadata {}, write_buffers {}, per_cf {}, block_cache {}, \
         charged_cfs {}) \
         pipeline={} (download_concurrency {}, queued_blocks {} MiB) \
         non_finalized={} (resident target {} MiB, index_clone_factor {}, not enforced) \
         network={} (peers {peers}, quic {}/{}/{} MiB) \
         mempool={tx_cost_limit} \
         rpc={} proposal cache \
         allocator arenas={} ({cores} cores)",
        MemoryBudget::Bytes(budget),
        plan.profile,
        mib_floor(plan.fixed),
        mib_floor(plan.slack),
        mib_floor(plan.shares.get(Component::RocksDb)),
        mib(ROCKSDB_PINNED_METADATA_BYTES),
        mib(rocksdb.write_buffer_total.unwrap_or_default()),
        mib(rocksdb.effective_write_buffer_size().unwrap_or_default()),
        mib(rocksdb.block_cache_bytes.unwrap_or_default()),
        rocksdb.charge_index_and_filter_cfs.len(),
        mib_floor(plan.shares.get(Component::Pipeline)),
        config.sync.download_concurrency_limit,
        usize_mib(plan.state.queued_blocks_bytes),
        mib_floor(plan.shares.get(Component::NonFinalized)),
        mib(plan.state.non_finalized_resident_bytes.unwrap_or_default()),
        settings.inputs.index_clone_factor,
        mib_floor(plan.shares.get(Component::Network)),
        u32_mib(quic.stream_receive_window),
        u32_mib(quic.connection_receive_window),
        mib(quic.send_window),
        mib_floor(plan.shares.get(Component::Rpc)),
        plan.allocator
            .arena_max
            .map_or_else(|| "default".to_string(), |arenas| arenas.to_string()),
    )
}

/// The memory budget of a running node: its settings, and the plan it publishes.
#[derive(Debug)]
pub struct MemoryResources {
    /// The budget's settings, or `None` without a budget.
    pub settings: Option<BudgetSettings>,

    /// Publishes the plan, and re-publishes it when the profile changes.
    pub plan_sender: MemoryPlanSender,
}

impl MemoryResources {
    /// Returns the current plan.
    pub fn plan(&self) -> MemoryPlan {
        self.plan_sender.borrow().clone()
    }

    /// Returns a receiver of the plan and its changes.
    pub fn subscribe(&self) -> MemoryPlanReceiver {
        self.plan_sender.subscribe()
    }
}

/// Resolves the memory budget of `config` into the plan the node opens with, and logs the plan
/// and every warning about it (design §2.4).
///
/// Without a budget, the plan is [`MemoryPlan::default()`]: every component keeps today's
/// limits. This reads `/proc` and `/sys/fs/cgroup` for an `"auto"` budget, so async callers run
/// it on a blocking thread.
///
/// # Errors
///
/// If the budget is below the floors (without `--unsafe-low-specs`), or an `"auto"` budget
/// cannot be detected or is below the initial sync minimum.
pub fn init(config: &ZebradConfig, unsafe_low_specs: bool) -> Result<MemoryResources, Report> {
    let Some((settings, warnings)) =
        BudgetSettings::from_config(config, unsafe_low_specs, detect::detect)?
    else {
        let (plan_sender, _) = watch::channel(MemoryPlan::default());
        return Ok(MemoryResources {
            settings: None,
            plan_sender,
        });
    };

    for warning in warnings {
        tracing::warn!("{warning}");
    }

    let (plan, warnings) = settings.plan(settings.initial_profile())?;
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    tracing::info!("{}", startup_summary(&plan, &settings, config));
    if let Some(tx_cost_limit) = plan.mempool.tx_cost_limit {
        let default = mempool::Config::default().tx_cost_limit;
        if tx_cost_limit < default {
            tracing::info!(
                "the memory budget lowers [mempool] tx_cost_limit to {} from ZIP-401's \
                 recommended {}",
                with_thousands_separators(tx_cost_limit),
                with_thousands_separators(default),
            );
        }
    }

    let (plan_sender, _) = watch::channel(plan);

    Ok(MemoryResources {
        settings: Some(settings),
        plan_sender,
    })
}
