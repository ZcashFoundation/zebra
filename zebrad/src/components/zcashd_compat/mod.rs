//! zcashd-compat mode configuration and `zcashd` child-process supervision.

mod config;
mod supervisor;

pub use config::Config;
pub use supervisor::{is_command_resolvable, run as run_supervisor, SupervisorConfig};
