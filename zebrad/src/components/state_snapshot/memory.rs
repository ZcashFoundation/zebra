//! Memory limits for state-snapshot bootstrap, derived before any peer is dialed.

use zebra_network::QuicWindows;
use zebra_state::state_snapshot::format::MAX_PIECE_BYTES;

use crate::components::resources::MemoryPlan;

use super::scheduler::SchedulerConfig;

#[cfg(test)]
mod tests;

/// The most peers a bootstrap dials, with or without a memory budget.
const MAX_BOOTSTRAP_PEERS: usize = 8;

/// The memory a snapshot bootstrap uses before the state opens.
#[derive(Clone, Debug, Default)]
pub struct MemoryLimits {
    /// The piece scheduler's download and peer limits.
    pub scheduler: SchedulerConfig,

    /// The maximum concurrent SST builds, or `None` to use half the host's CPUs as before.
    pub sst_build_parallelism: Option<usize>,
}

impl MemoryLimits {
    /// Derives bootstrap limits from `plan`, or returns the unchanged defaults without a budget.
    ///
    /// Half of the pipeline share reserves downloaded pieces; the other half bounds the full piece
    /// buffers read by concurrent SST builds. Each side always admits one piece. The network share
    /// bounds the dialled peers by their QUIC connection receive and send windows.
    pub fn from_plan(plan: &MemoryPlan, cores: usize) -> Self {
        if plan.budget.is_none() {
            return Self::default();
        }

        let pipeline = plan
            .sync
            .pipeline_bytes
            .expect("every budgeted plan has a pipeline limit");
        let sst_bytes = pipeline / 2;
        let download_bytes = pipeline - sst_bytes;
        let piece_bytes = u64::try_from(MAX_PIECE_BYTES).expect("the piece limit fits in u64");
        let cpu_parallelism = (cores / 2).max(1);
        let byte_parallelism = (sst_bytes / piece_bytes).max(1);
        let sst_build_parallelism = usize::try_from(byte_parallelism)
            .unwrap_or(usize::MAX)
            .min(cpu_parallelism);

        let transport_bytes = u64::from(QuicWindows::BOOTSTRAP.connection_receive_window)
            .saturating_add(QuicWindows::BOOTSTRAP.send_window);
        let max_peers = (plan
            .shares
            .get(crate::components::resources::Component::Network)
            / transport_bytes)
            .clamp(
                1,
                u64::try_from(MAX_BOOTSTRAP_PEERS).expect("the peer limit is a small number"),
            );
        let max_peers = usize::try_from(max_peers).expect("the peer limit is at most eight");

        Self {
            scheduler: SchedulerConfig {
                budget_bytes: download_bytes,
                max_peers,
                ..SchedulerConfig::default()
            },
            sst_build_parallelism: Some(sst_build_parallelism),
        }
    }
}
