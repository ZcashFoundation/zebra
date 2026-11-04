//! The memory budget: one `[resources] memory_budget` setting from which zebrad derives the memory
//! limits of every component.
//!
//! Without a budget, which is the default, every component keeps today's limits.

pub mod config;
pub mod detect;
pub mod plan;

pub use config::{Config, MemoryBudget, ParseMemoryBudgetError};
pub use plan::{
    AllocatorLimits, AutoBudget, Component, ComponentShares, ComponentShortfall, MemoryPlan,
    MemoryPlanReceiver, MemoryPlanSender, MinimumBudgets, PlanInputs, Profile, Shortfall,
};
