//! State snapshot data stored in the finalized database.

use zebra_chain::{block, state_snapshot::ObjectHash};

/// Where a snapshot-imported database's history starts, and what it was
/// imported from: the value of the `snapshot_base` column family.
///
/// Blocks below `window_start` have no bodies and no hash rows; blocks from
/// `window_start` to `height` are complete; `height` is the snapshot height
/// `H`. The checks the importer could not run against the chain are recorded
/// in `deferred_checks`, so the node runs them when it can.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotBase {
    /// The first height with a complete block: `L = max(1, H - 2303)`.
    pub window_start: block::Height,

    /// The snapshot height `H`.
    pub height: block::Height,

    /// The hash of block `H`.
    pub block_hash: block::Hash,

    /// The hash of the manifest the database was imported from.
    pub manifest: ObjectHash,

    /// The checks against the chain that are still pending, as a set of the
    /// `DEFERRED_*` bits.
    pub deferred_checks: u8,
}

impl SnapshotBase {
    /// `hash(H)` has not been checked against the chain: `H` is not a
    /// checkpoint height, and no bootstrap peer served the headers that chain
    /// it to the next checkpoint, so the checkpoint verifier anchors the next
    /// checkpoint back to it instead.
    pub const DEFERRED_BLOCK_HASH: u8 = 1 << 0;

    /// Block `H + 1`'s commitments have not been checked against the
    /// snapshot's history tree and tree roots: no bootstrap peer served the
    /// block, so the first commit above `H` checks them.
    pub const DEFERRED_SUCCESSOR_COMMITMENTS: u8 = 1 << 1;

    /// Returns whether every check in `checks` is still pending.
    pub fn has_deferred(&self, checks: u8) -> bool {
        self.deferred_checks & checks == checks
    }
}
