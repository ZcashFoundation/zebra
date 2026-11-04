//! The memory plan: the memory limits of every component, derived from the memory budget.

use std::fmt;

use tokio::sync::watch;

use zebra_chain::parameters::checkpoint::constants::MAX_CHECKPOINT_BYTE_COUNT;

use crate::components::{
    mempool::MempoolMemoryLimits,
    resources::MemoryBudget,
    sync::{SyncMemoryLimits, MAX_CHECKPOINT_CHARGE_RATIO},
};

#[cfg(test)]
mod tests;

/// One mebibyte, in bytes.
const MIB: u64 = 1024 * 1024;

/// One gibibyte, in bytes.
const GIB: u64 = 1024 * MIB;

/// The process memory a budget reserves for costs that no limit shrinks: the runtimes, the
/// verifying keys and Sapling parameters, and the state at open (design §2.3 step 1, D10).
pub const FIXED_FLOOR_BYTES: u64 = 384 * MIB;

/// The percentage of the budget held back for allocator retention, transient deserialization
/// and RocksDB's soft overshoot (design §2.3 step 2).
pub const SLACK_PERCENT: u64 = 15;

/// How many copies of the non-finalized index maps a commit or a fork can hold at once, until
/// they are copy-on-write (design §2.3, §3.9).
pub const INDEX_CLONE_FACTOR: u64 = 2;

/// The heap-pinned SST index and filter blocks of the column families the block commit path
/// point-reads, and the table reader structs of a full Mainnet database: the part of RocksDB's
/// share that no RocksDB option bounds (design §2.3, §4.1).
pub const ROCKSDB_PINNED_METADATA_BYTES: u64 = 160 * MIB;

/// The smallest RocksDB write buffer manager total a plan derives.
pub const ROCKSDB_MIN_WRITE_BUFFER_BYTES: u64 = 64 * MIB;

/// The smallest RocksDB block cache a plan derives.
pub const ROCKSDB_MIN_BLOCK_CACHE_BYTES: u64 = 32 * MIB;

/// The percentage of RocksDB's bounded share, above its pinned metadata, that goes to the write
/// buffer manager; the block cache gets the rest.
const ROCKSDB_WRITE_BUFFER_PERCENT: u64 = 60;

/// The `TipOnly` sync pipeline floor: a few full-verification blocks in flight.
const TIP_ONLY_PIPELINE_FLOOR_BYTES: u64 = 32 * MIB;

/// The percentage of the components' memory the `TipOnly` sync pipeline can use at most.
const TIP_ONLY_PIPELINE_MAX_PERCENT: u64 = 25;

/// The percentage of the surplus from a component's ceiling that goes to RocksDB in the `Ibd`
/// profile; the sync pipeline gets the rest. In the `TipOnly` profile, RocksDB gets all of it.
const IBD_SURPLUS_TO_ROCKSDB_PERCENT: u64 = 60;

/// The percentage of the nominal shares moved from the sync pipeline to the RPC proposal cache
/// when the node mines or serves block templates (design §2.3 step 3).
const MINING_SHARE_PERCENT: u64 = 5;

/// The network share per peer in the initial target size.
const NETWORK_SHARE_PER_PEER_BYTES: u64 = 8 * MIB;

/// The smallest initial peer target size a plan derives.
const MIN_PEERSET_INITIAL_TARGET_SIZE: u64 = 8;

/// The network share at and above which the QUIC windows keep their default sizes; below it,
/// the plan uses [`zebra_network::QuicWindows::compact`].
const QUIC_DEFAULT_WINDOWS_MIN_SHARE_BYTES: u64 = 256 * MIB;

/// The mempool transaction cost units per two and a half bytes of the mempool share: in-memory
/// transactions take 1.5 to 2.5 times their serialized size (design §2.3, R1 §2.3).
const MEMPOOL_COST_PER_SHARE_BYTE_NUMERATOR: u64 = 2;

/// See [`MEMPOOL_COST_PER_SHARE_BYTE_NUMERATOR`].
const MEMPOOL_COST_PER_SHARE_BYTE_DENOMINATOR: u64 = 5;

/// The smallest mempool transaction cost limit a plan derives.
const MIN_MEMPOOL_TX_COST_LIMIT: u64 = 10_000_000;

/// The largest mempool transaction cost limit a plan derives: ZIP-401's recommended default.
const MAX_MEMPOOL_TX_COST_LIMIT: u64 = 80_000_000;

/// The smallest state orphan queue bound a plan derives.
const MIN_QUEUED_BLOCKS_BYTES: u64 = 64 * MIB;

/// The fewest and most glibc malloc arenas a plan derives (design §4.7, P8).
const MIN_ARENA_MAX: usize = 2;

/// See [`MIN_ARENA_MAX`].
const MAX_ARENA_MAX: usize = 16;

/// The CPU cores per glibc malloc arena (design §4.7, P8).
const CORES_PER_ARENA: usize = 4;

/// The largest budget `"auto"` derives.
pub const MAX_AUTO_BUDGET_BYTES: u64 = 8 * GIB;

/// The percentage of the effective memory `"auto"` budgets.
const AUTO_BUDGET_PERCENT: u64 = 50;

/// The memory a budget should leave the host, which the oversize warning uses (design §2.2).
const HOST_RESERVED_BYTES: u64 = 512 * MIB;

/// The node's memory profile, which sets the components' floors and shares.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Profile {
    /// Initial block download: the sync pipeline gets a large share, with a floor of one
    /// checkpoint range.
    ///
    /// Also the profile of a node without a budget.
    #[default]
    Ibd,

    /// At the chain tip: the sync pipeline gets a small share, and the rest goes to the other
    /// components.
    TipOnly,
}

impl Profile {
    /// The profile's name in logs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ibd => "ibd",
            Self::TipOnly => "tip_only",
        }
    }

    /// The value of the `zebra.memory.profile` metric: 0 for `TipOnly`, 1 for `Ibd`.
    pub fn metric_value(self) -> f64 {
        match self {
            Self::Ibd => 1.0,
            Self::TipOnly => 0.0,
        }
    }

    /// The profile a budget opens the node with: `Ibd` when the budget covers its floors, or
    /// `TipOnly` (design §2.3 step 3).
    pub fn for_budget(budget: u64, inputs: &PlanInputs) -> Self {
        if budget >= MemoryPlan::min_budget(Self::Ibd, inputs) {
            Self::Ibd
        } else {
            Self::TipOnly
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A component with a share of the memory budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Component {
    /// The finalized state's RocksDB database.
    RocksDb,

    /// The block sync pipeline.
    Pipeline,

    /// The non-finalized state.
    NonFinalized,

    /// Peer connections.
    Network,

    /// The mempool.
    Mempool,

    /// The RPC server's block proposal cache.
    Rpc,
}

impl Component {
    /// Every component, in the order the plan lists them.
    pub const ALL: [Self; 6] = [
        Self::RocksDb,
        Self::Pipeline,
        Self::NonFinalized,
        Self::Network,
        Self::Mempool,
        Self::Rpc,
    ];

    /// The component's label in the `zebra.memory.*` metrics.
    pub fn label(self) -> &'static str {
        match self {
            Self::RocksDb => "rocksdb",
            Self::Pipeline => "pipeline",
            Self::NonFinalized => "non_finalized",
            Self::Network => "network",
            Self::Mempool => "mempool",
            Self::Rpc => "rpc",
        }
    }

    /// The component's name in messages.
    fn name(self) -> &'static str {
        match self {
            Self::RocksDb => "RocksDB",
            Self::Pipeline => "the sync pipeline",
            Self::NonFinalized => "the non-finalized state",
            Self::Network => "the network",
            Self::Mempool => "the mempool",
            Self::Rpc => "the RPC proposal cache",
        }
    }

    /// The component's index in [`Component::ALL`].
    fn index(self) -> usize {
        match self {
            Self::RocksDb => 0,
            Self::Pipeline => 1,
            Self::NonFinalized => 2,
            Self::Network => 3,
            Self::Mempool => 4,
            Self::Rpc => 5,
        }
    }

    /// The least memory the component needs in `profile`, in bytes (design §2.3).
    pub fn floor(self, profile: Profile) -> u64 {
        match (self, profile) {
            (Self::RocksDb, _) => {
                ROCKSDB_PINNED_METADATA_BYTES
                    + ROCKSDB_MIN_WRITE_BUFFER_BYTES
                    + ROCKSDB_MIN_BLOCK_CACHE_BYTES
            }
            // The oldest checkpoint range is always admitted (design §4.2 step 2).
            (Self::Pipeline, Profile::Ibd) => {
                MAX_CHECKPOINT_BYTE_COUNT * MAX_CHECKPOINT_CHARGE_RATIO
            }
            (Self::Pipeline, Profile::TipOnly) => TIP_ONLY_PIPELINE_FLOOR_BYTES,
            (Self::NonFinalized, _) => 64 * MIB,
            (Self::Network, _) => 32 * MIB,
            // The memory of a 10 million cost mempool.
            (Self::Mempool, _) => 25 * MIB,
            (Self::Rpc, _) => 0,
        }
    }

    /// The most memory the plan gives the component, in bytes (design §2.3).
    pub fn ceiling(self) -> u64 {
        match self {
            Self::RocksDb => 4 * GIB,
            Self::Pipeline | Self::NonFinalized => 2 * GIB,
            Self::Network | Self::Rpc => 512 * MIB,
            // The memory of an 80 million cost mempool.
            Self::Mempool => 200 * MIB,
        }
    }

    /// The component's nominal percentage of the components' memory (design §2.3).
    ///
    /// In the `TipOnly` profile the pipeline has no nominal share, and the other components
    /// split what it leaves by these percentages, renormalized.
    fn nominal_percent(self, mining: bool) -> u64 {
        let mining_share = if mining { MINING_SHARE_PERCENT } else { 0 };

        match self {
            Self::RocksDb => 35,
            Self::Pipeline => 25 - mining_share,
            Self::NonFinalized => 15,
            Self::Network | Self::Mempool => 10,
            Self::Rpc => 5 + mining_share,
        }
    }
}

/// The memory each component gets from a plan, in bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct ComponentShares([u64; 6]);

impl ComponentShares {
    /// Returns the shares where every component gets its floor in `profile`.
    pub fn floors(profile: Profile) -> Self {
        Self(Component::ALL.map(|component| component.floor(profile)))
    }

    /// Returns `component`'s share, in bytes.
    pub fn get(&self, component: Component) -> u64 {
        self.0[component.index()]
    }

    /// Returns the sum of the shares, in bytes.
    pub fn total(&self) -> u64 {
        self.0
            .iter()
            .fold(0, |total, share| total.saturating_add(*share))
    }

    /// Returns every component with its share.
    pub fn iter(&self) -> impl Iterator<Item = (Component, u64)> + '_ {
        Component::ALL
            .into_iter()
            .map(|component| (component, self.get(component)))
    }
}

/// The inputs of [`MemoryPlan::derive`] other than the budget and profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanInputs {
    /// The memory reserved for fixed costs, in bytes; [`FIXED_FLOOR_BYTES`] by default.
    pub fixed_floor_bytes: u64,

    /// The percentage of the budget held back as slack; [`SLACK_PERCENT`] by default.
    pub slack_percent: u64,

    /// The number of CPU cores the node uses.
    pub cores: usize,

    /// The non-finalized index copies to budget for; [`INDEX_CLONE_FACTOR`] by default.
    pub index_clone_factor: u64,

    /// The moving average of the sync pipeline's charge per block, in bytes, once the syncer
    /// has measured it.
    pub ewma_charge: Option<u64>,

    /// Whether the node mines or serves block templates, which moves part of the sync
    /// pipeline's share to the RPC proposal cache.
    pub mining: bool,

    /// The number of blocks the syncer verifies at once after the last checkpoint, which with
    /// `ewma_charge` sizes the `TipOnly` sync pipeline.
    pub full_verify_concurrency_limit: usize,
}

impl Default for PlanInputs {
    fn default() -> Self {
        Self {
            fixed_floor_bytes: FIXED_FLOOR_BYTES,
            slack_percent: SLACK_PERCENT,
            cores: 1,
            index_clone_factor: INDEX_CLONE_FACTOR,
            ewma_charge: None,
            mining: false,
            full_verify_concurrency_limit: crate::components::sync::Config::default()
                .full_verify_concurrency_limit,
        }
    }
}

/// Memory limits for the allocator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AllocatorLimits {
    /// The most glibc malloc arenas.
    ///
    /// `None` keeps the cap `zebrad` sets at startup, or the operator's `MALLOC_ARENA_MAX`.
    pub arena_max: Option<usize>,
}

impl AllocatorLimits {
    /// Returns the glibc malloc arena cap for a node using `cores` CPU cores:
    /// `clamp(cores / 4, 2, 16)` (design §4.7, P8).
    pub fn arena_max_for_cores(cores: usize) -> usize {
        (cores / CORES_PER_ARENA).clamp(MIN_ARENA_MAX, MAX_ARENA_MAX)
    }
}

/// A component whose floor its share of a budget does not cover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComponentShortfall {
    /// The component.
    pub component: Component,

    /// The least memory the component needs, in bytes.
    pub floor: u64,

    /// The memory the budget's share gives it, in bytes.
    pub share: u64,
}

/// A budget below the least memory the node needs in a profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shortfall {
    /// The smallest budget that covers every floor in `profile`, in bytes.
    pub needed: u64,

    /// The budget, in bytes.
    pub available: u64,

    /// Every component whose floor the budget does not cover.
    pub per_component: Vec<ComponentShortfall>,

    /// The profile the budget was derived for.
    pub profile: Profile,

    /// The smallest budgets of both profiles, in bytes, which the refusal names.
    pub minimums: MinimumBudgets,
}

/// The smallest budget of each profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinimumBudgets {
    /// The smallest budget of a node at the tip, in bytes.
    pub tip_only: u64,

    /// The smallest budget of a node in initial sync, in bytes.
    pub ibd: u64,
}

impl MinimumBudgets {
    /// Returns the smallest budgets of both profiles with `inputs`.
    pub fn new(inputs: &PlanInputs) -> Self {
        Self {
            tip_only: MemoryPlan::min_budget(Profile::TipOnly, inputs),
            ibd: MemoryPlan::min_budget(Profile::Ibd, inputs),
        }
    }
}

impl fmt::Display for Shortfall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let budget = MemoryBudget::Bytes(self.available);
        let tip_only = mib_ceil(self.minimums.tip_only);
        let ibd = mib_ceil(self.minimums.ibd);

        match self.profile {
            Profile::TipOnly => write!(
                f,
                "memory_budget {budget} is below the {tip_only} minimum for a node at the tip \
                 ({ibd} for initial sync): "
            )?,
            Profile::Ibd => write!(
                f,
                "memory_budget {budget} is below the {ibd} minimum for initial sync \
                 ({tip_only} for a node at the tip): "
            )?,
        }

        for (index, shortfall) in self.per_component.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(
                f,
                "{} needs {} but its share is {}",
                shortfall.component.name(),
                mib_ceil(shortfall.floor),
                mib_floor(shortfall.share),
            )?;
        }

        f.write_str(
            "; raise the budget, remove it, sync from a snapshot, or pass --unsafe-low-specs",
        )
    }
}

impl std::error::Error for Shortfall {}

/// The budget `"auto"` derives from the memory available to Zebra.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoBudget {
    /// The budget, in bytes.
    pub budget: u64,

    /// Whether half of the effective memory was below the `Ibd` minimum, so the budget was
    /// raised to the minimum.
    pub raised_to_minimum: bool,
}

/// The memory limits of every component, derived from the memory budget.
///
/// [`MemoryPlan::default()`] is the plan of a node without a budget: every limit is today's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryPlan {
    /// The budget, in bytes, or `None` without a budget.
    pub budget: Option<u64>,

    /// The profile the limits were derived for.
    pub profile: Profile,

    /// The memory reserved for fixed costs, in bytes.
    pub fixed: u64,

    /// The memory held back as slack, in bytes.
    pub slack: u64,

    /// The memory each component gets, in bytes, which the limits below are derived from.
    ///
    /// All zero without a budget.
    pub shares: ComponentShares,

    /// The state's limits, including RocksDB's.
    pub state: zebra_state::MemoryLimits,

    /// The network's limits.
    pub network: zebra_network::MemoryLimits,

    /// The sync pipeline's limits.
    pub sync: SyncMemoryLimits,

    /// The mempool's limits.
    pub mempool: MempoolMemoryLimits,

    /// The RPC share: the retained bytes of cached block proposals, passed to
    /// [`zebra_consensus::router::init_with_limits`].
    pub proposal_cache_bytes: usize,

    /// The allocator's limits.
    pub allocator: AllocatorLimits,
}

impl Default for MemoryPlan {
    fn default() -> Self {
        Self {
            budget: None,
            profile: Profile::default(),
            fixed: 0,
            slack: 0,
            shares: ComponentShares::default(),
            state: zebra_state::MemoryLimits::default(),
            network: zebra_network::MemoryLimits::default(),
            sync: SyncMemoryLimits::default(),
            mempool: MempoolMemoryLimits::default(),
            proposal_cache_bytes: zebra_consensus::DEFAULT_PROPOSAL_CACHE_BYTES,
            allocator: AllocatorLimits::default(),
        }
    }
}

impl MemoryPlan {
    /// Derives the limits of every component from a `budget` in bytes, for `profile`
    /// (design §2.3).
    ///
    /// The budget less the fixed floor and the slack is split between the components by their
    /// nominal shares. A component below its floor is raised to it, taking the deficit from
    /// the other components in proportion to their headroom above their own floors; a
    /// component above its ceiling is lowered to it, and the surplus goes to RocksDB and the
    /// sync pipeline (`Ibd`) or to RocksDB (`TipOnly`), up to their ceilings.
    ///
    /// This is a pure function: the same arguments always derive the same plan.
    ///
    /// # Errors
    ///
    /// If `budget` does not cover the floors of `profile`.
    pub fn derive(budget: u64, profile: Profile, inputs: &PlanInputs) -> Result<Self, Shortfall> {
        let shares = Self::shares(budget, profile, inputs)?;

        Ok(Self::from_shares(budget, profile, inputs, shares))
    }

    /// Returns the plan where every component gets its floor in `profile`, for a `budget`
    /// below the floors that the operator runs with anyway (`--unsafe-low-specs`, design §2.3
    /// step 5).
    pub fn clamped_to_floors(budget: u64, profile: Profile, inputs: &PlanInputs) -> Self {
        Self::from_shares(budget, profile, inputs, ComponentShares::floors(profile))
    }

    /// Returns the smallest budget whose plan covers every floor of `profile`, in bytes.
    ///
    /// Derived from the floors, the fixed floor and the slack, so that it changes with them:
    /// `ceil((Σ floors + fixed) / (1 − slack))` (design §2.2).
    pub fn min_budget(profile: Profile, inputs: &PlanInputs) -> u64 {
        let needed = ComponentShares::floors(profile).total();

        // The components' memory never decreases as the budget grows, so the smallest budget
        // that covers the floors is found by bisection over every budget.
        let (mut low, mut high) = (0, u64::MAX);
        if Self::components_memory(high, inputs) < needed {
            return u64::MAX;
        }
        while low < high {
            let middle = low + (high - low) / 2;
            if Self::components_memory(middle, inputs) >= needed {
                high = middle;
            } else {
                low = middle + 1;
            }
        }

        low
    }

    /// Returns the budget `"auto"` derives from `effective` bytes of memory available to Zebra:
    /// half of it, at least the `Ibd` minimum and at most [`MAX_AUTO_BUDGET_BYTES`]
    /// (design §2.2, C8).
    ///
    /// # Errors
    ///
    /// If `effective` is below the `Ibd` minimum.
    pub fn auto_budget(effective: u64, inputs: &PlanInputs) -> Result<AutoBudget, Shortfall> {
        let minimum = Self::min_budget(Profile::Ibd, inputs);
        if effective < minimum {
            return Err(Self::shares(effective, Profile::Ibd, inputs)
                .expect_err("a budget below the minimum does not cover the floors"));
        }

        let half = mul_div_floor(effective, AUTO_BUDGET_PERCENT, 100);
        let budget = half.clamp(minimum, MAX_AUTO_BUDGET_BYTES.max(minimum));

        Ok(AutoBudget {
            budget,
            raised_to_minimum: half < minimum,
        })
    }

    /// Returns whether `budget` leaves the host too little of its `effective` memory: more than
    /// three quarters of what remains after 512 MiB for the host (design §2.2).
    pub fn is_oversized(budget: u64, effective: u64) -> bool {
        let usable = effective.saturating_sub(HOST_RESERVED_BYTES);

        budget > mul_div_floor(usable, 3, 4)
    }

    /// Returns the slack of `budget` with `inputs`, rounded up so that the components never
    /// get more than the budget allows.
    fn slack(budget: u64, inputs: &PlanInputs) -> u64 {
        mul_div_ceil(budget, inputs.slack_percent, 100).min(budget)
    }

    /// Returns the memory the components share: the budget less the fixed floor and the slack.
    fn components_memory(budget: u64, inputs: &PlanInputs) -> u64 {
        budget
            .saturating_sub(inputs.fixed_floor_bytes)
            .saturating_sub(Self::slack(budget, inputs))
    }

    /// Returns each component's nominal share of `components_memory`, before floors and
    /// ceilings.
    fn nominal_shares(components_memory: u64, profile: Profile, inputs: &PlanInputs) -> [u64; 6] {
        let mut nominal = [0; 6];

        match profile {
            Profile::Ibd => {
                for component in Component::ALL {
                    nominal[component.index()] = mul_div_floor(
                        components_memory,
                        component.nominal_percent(inputs.mining),
                        100,
                    );
                }
            }
            Profile::TipOnly => {
                // The tip pipeline holds a lookahead of full-verification blocks, 32 MiB until
                // the syncer has measured their charge (design §2.3 step 4).
                let floor = Component::Pipeline.floor(profile);
                let estimate = inputs.ewma_charge.map_or(floor, |charge| {
                    charge.saturating_mul(
                        u64::try_from(inputs.full_verify_concurrency_limit).unwrap_or(u64::MAX),
                    )
                });
                let cap =
                    mul_div_floor(components_memory, TIP_ONLY_PIPELINE_MAX_PERCENT, 100).max(floor);
                let pipeline = estimate.max(floor).min(cap).min(components_memory);
                nominal[Component::Pipeline.index()] = pipeline;

                let others = components_memory - pipeline;
                let others_percent: u64 = Component::ALL
                    .into_iter()
                    .filter(|component| *component != Component::Pipeline)
                    .map(|component| component.nominal_percent(inputs.mining))
                    .sum();
                for component in Component::ALL {
                    if component != Component::Pipeline {
                        nominal[component.index()] = mul_div_floor(
                            others,
                            component.nominal_percent(inputs.mining),
                            others_percent,
                        );
                    }
                }
            }
        }

        nominal
    }

    /// Returns the memory each component gets from `budget` in `profile`.
    fn shares(
        budget: u64,
        profile: Profile,
        inputs: &PlanInputs,
    ) -> Result<ComponentShares, Shortfall> {
        let components_memory = Self::components_memory(budget, inputs);
        let floors = ComponentShares::floors(profile);
        let nominal = Self::nominal_shares(components_memory, profile, inputs);

        if components_memory < floors.total() {
            return Err(Shortfall {
                needed: Self::min_budget(profile, inputs),
                available: budget,
                per_component: Component::ALL
                    .into_iter()
                    .filter(|component| nominal[component.index()] < floors.get(*component))
                    .map(|component| ComponentShortfall {
                        component,
                        floor: floors.get(component),
                        share: nominal[component.index()],
                    })
                    .collect(),
                profile,
                minimums: MinimumBudgets::new(inputs),
            });
        }

        // Raise every component to its floor, taking the deficit from the components above
        // their floors in proportion to their headroom. Each of those keeps
        // `floor + headroom × (Σ nominal − Σ floors) / Σ headroom`, which never decreases as the
        // budget grows, and the shares never sum to more than the nominal shares.
        let nominal_total: u64 = nominal.iter().sum();
        let spare = nominal_total.saturating_sub(floors.total());
        let headroom_total: u64 = Component::ALL
            .into_iter()
            .map(|component| nominal[component.index()].saturating_sub(floors.get(component)))
            .sum();

        let mut shares = floors.0;
        for component in Component::ALL {
            let headroom = nominal[component.index()].saturating_sub(floors.get(component));
            if headroom > 0 {
                shares[component.index()] += mul_div_floor(headroom, spare, headroom_total);
            }
        }

        // Lower every component to its ceiling, and give the surplus to the components that
        // can grow most usefully.
        let mut surplus = 0;
        for component in Component::ALL {
            let share = &mut shares[component.index()];
            surplus += share.saturating_sub(component.ceiling());
            *share = (*share).min(component.ceiling());
        }

        let (to_rocksdb, to_pipeline) = match profile {
            Profile::Ibd => {
                let to_rocksdb = mul_div_floor(surplus, IBD_SURPLUS_TO_ROCKSDB_PERCENT, 100);
                (to_rocksdb, surplus - to_rocksdb)
            }
            Profile::TipOnly => (surplus, 0),
        };
        let unused = grow(&mut shares, Component::RocksDb, to_rocksdb);
        if profile == Profile::Ibd {
            let unused = grow(&mut shares, Component::Pipeline, to_pipeline + unused);
            grow(&mut shares, Component::RocksDb, unused);
        }

        Ok(ComponentShares(shares))
    }

    /// Returns the plan that gives each component its `shares`.
    fn from_shares(
        budget: u64,
        profile: Profile,
        inputs: &PlanInputs,
        shares: ComponentShares,
    ) -> Self {
        let rocksdb = shares.get(Component::RocksDb);
        let pipeline = shares.get(Component::Pipeline);
        let network = shares.get(Component::Network);

        // RocksDB's bounded memory, above its pinned metadata, is split between the write
        // buffer manager and the block cache, each at least its floor (design §2.3, §4.1).
        let rocksdb_bounded = rocksdb.saturating_sub(ROCKSDB_PINNED_METADATA_BYTES);
        let write_buffer_total = mul_div_floor(rocksdb_bounded, ROCKSDB_WRITE_BUFFER_PERCENT, 100)
            .max(ROCKSDB_MIN_WRITE_BUFFER_BYTES);
        let block_cache_bytes = rocksdb_bounded
            .saturating_sub(write_buffer_total)
            .max(ROCKSDB_MIN_BLOCK_CACHE_BYTES);

        let default_state = zebra_state::MemoryLimits::default();
        let queued_blocks_bytes = pipeline.clamp(
            MIN_QUEUED_BLOCKS_BYTES,
            u64::try_from(default_state.queued_blocks_bytes).unwrap_or(u64::MAX),
        );

        let peers = (network / NETWORK_SHARE_PER_PEER_BYTES).clamp(
            MIN_PEERSET_INITIAL_TARGET_SIZE,
            u64::try_from(zebra_network::constants::DEFAULT_PEERSET_INITIAL_TARGET_SIZE)
                .expect("the default peer target is a small number"),
        );

        let tx_cost_limit = mul_div_floor(
            shares.get(Component::Mempool),
            MEMPOOL_COST_PER_SHARE_BYTE_NUMERATOR,
            MEMPOOL_COST_PER_SHARE_BYTE_DENOMINATOR,
        )
        .clamp(MIN_MEMPOOL_TX_COST_LIMIT, MAX_MEMPOOL_TX_COST_LIMIT);

        Self {
            budget: Some(budget),
            profile,
            fixed: inputs.fixed_floor_bytes,
            slack: Self::slack(budget, inputs),
            shares,
            state: zebra_state::MemoryLimits {
                rocksdb: zebra_state::RocksDbMemoryLimits {
                    write_buffer_total: Some(write_buffer_total),
                    block_cache_bytes: Some(block_cache_bytes),
                    charge_index_and_filter_cfs: zebra_state::READ_PATH_COLUMN_FAMILIES,
                    ..zebra_state::RocksDbMemoryLimits::default()
                },
                non_finalized_resident_bytes: Some(
                    shares.get(Component::NonFinalized) / inputs.index_clone_factor.max(1),
                ),
                queued_blocks_bytes: usize::try_from(queued_blocks_bytes)
                    .expect("the queued blocks bound is at most the default, which is a usize"),
            },
            network: zebra_network::MemoryLimits {
                peerset_initial_target_size: Some(
                    usize::try_from(peers).expect("the peer target is at most 25"),
                ),
                quic: if network >= QUIC_DEFAULT_WINDOWS_MIN_SHARE_BYTES {
                    zebra_network::QuicWindows::default()
                } else {
                    zebra_network::QuicWindows::compact()
                },
            },
            sync: SyncMemoryLimits {
                pipeline_bytes: Some(pipeline),
            },
            mempool: MempoolMemoryLimits {
                tx_cost_limit: Some(tx_cost_limit),
            },
            proposal_cache_bytes: usize::try_from(shares.get(Component::Rpc)).unwrap_or(usize::MAX),
            allocator: AllocatorLimits {
                arena_max: Some(AllocatorLimits::arena_max_for_cores(inputs.cores)),
            },
        }
    }
}

/// Adds up to `extra` bytes to `component`'s share without passing its ceiling, and returns
/// the bytes it could not add.
fn grow(shares: &mut [u64; 6], component: Component, extra: u64) -> u64 {
    let share = &mut shares[component.index()];
    let added = extra.min(component.ceiling().saturating_sub(*share));
    *share += added;

    extra - added
}

/// Returns `value × numerator / denominator`, rounded down, or 0 for a zero denominator.
fn mul_div_floor(value: u64, numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        return 0;
    }

    let result = u128::from(value) * u128::from(numerator) / u128::from(denominator);
    u64::try_from(result).unwrap_or(u64::MAX)
}

/// Returns `value × numerator / denominator`, rounded up, or 0 for a zero denominator.
fn mul_div_ceil(value: u64, numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        return 0;
    }

    let result = (u128::from(value) * u128::from(numerator)).div_ceil(u128::from(denominator));
    u64::try_from(result).unwrap_or(u64::MAX)
}

/// Formats `bytes` as whole mebibytes, rounded up, with thousands separators: `"1,084 MiB"`.
pub fn mib_ceil(bytes: u64) -> String {
    format!("{} MiB", with_thousands_separators(bytes.div_ceil(MIB)))
}

/// Formats `bytes` as whole mebibytes, rounded down, with thousands separators: `"339 MiB"`.
pub fn mib_floor(bytes: u64) -> String {
    format!("{} MiB", with_thousands_separators(bytes / MIB))
}

/// Formats `value` with a comma between every three digits: `"54,000,000"`.
pub fn with_thousands_separators(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }

    formatted
}

/// Publishes the memory plan, and re-publishes it when the profile changes.
pub type MemoryPlanSender = watch::Sender<MemoryPlan>;

/// Receives the memory plan and its changes.
pub type MemoryPlanReceiver = watch::Receiver<MemoryPlan>;
