//! Descriptions of the metrics `zebrad` itself records.
//!
//! The library crates describe their own metrics in their `metrics` modules, and
//! [`describe_all`](super::describe_all) describes every one of them once a recorder is installed.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// The label keys of each labelled metric `zebrad` records.
///
/// - `zebrad.build.info`: `version`, the `zebrad` version.
/// - `sync.restart.count`:
///   - `reason`: `exhausted_tips`, `obtain_tips_timeout`, `obtain_tips_error`,
///     `extend_tips_error`, `sync_step_timeout`, or `block_error`.
///   - `made_progress`: `true` or `false`.
/// - `sync.block.error.count`: `kind`, a block download or verify error kind such as
///   `not_found`, `download_timed_out`, `invalid`, `verify_timeout`, or `already_verified`.
/// - `sync.block.download.duration_seconds`: `result`, `success` or `cancelled`.
/// - `sync.block.verify.duration_seconds`: `result`, `success`, `failure`, or `cancelled`.
/// - `sync.stage.duration_seconds`: `stage`, `obtain_tips` or `extend_tips`.
/// - `mempool.failed.verify.tasks.total`: `reason`, one of `in_state`, `state_error`,
///   `download_failed`, `cancelled`, `invalid`, or `timeout`.
/// - `mempool.*.transactions.total`: `version`, the transaction version number, such as `4` or
///   `5`.
/// - `zcash.mempool.actions.unpaid`: `bk`, a fee weight ratio bucket: `< 0.2`, `< 0.4`, `< 0.6`,
///   `< 0.8`, or `< 1`.
/// - `zcash.mempool.size.weighted`: `bk`, a fee weight ratio bucket: `< 1`, `1`, `> 1`, `> 2`, or
///   `> 3`.
pub const LABELS: &[(&str, &[&str])] = &[
    ("zebrad.build.info", &["version"]),
    ("sync.restart.count", &["reason", "made_progress"]),
    ("sync.block.error.count", &["kind"]),
    ("sync.block.download.duration_seconds", &["result"]),
    ("sync.block.verify.duration_seconds", &["result"]),
    ("sync.stage.duration_seconds", &["stage"]),
    ("mempool.failed.verify.tasks.total", &["reason"]),
    ("mempool.downloaded.transactions.total", &["version"]),
    ("mempool.pushed.transactions.total", &["version"]),
    ("mempool.verified.transactions.total", &["version"]),
    ("zcash.mempool.actions.unpaid", &["bk"]),
    ("zcash.mempool.size.weighted", &["bk"]),
];

/// Describes every metric `zebrad` itself records to the installed [`metrics`] recorder.
pub fn describe() {
    describe_counter!(
        concat!(env!("CARGO_PKG_NAME"), ".build.info"),
        Unit::Count,
        "Always 1, with the running zebrad version as a label."
    );

    describe_sync();
    describe_gossip();
    describe_mempool();

    // zcashd compatibility mode
    describe_gauge!(
        "zcashd.compat.supervisor.active",
        Unit::Count,
        "1 while zebrad is supervising a zcashd child process, otherwise 0."
    );
    describe_gauge!(
        "zcashd.compat.supervisor.disabled",
        Unit::Count,
        "1 when zcashd supervision is turned off or has stopped, otherwise 0."
    );
    describe_gauge!(
        "zcashd.compat.supervisor.exhausted",
        Unit::Count,
        "1 when zcashd supervision stopped after too many restarts, otherwise 0."
    );
}

/// Describes the syncer's metrics.
fn describe_sync() {
    describe_counter!(
        "sync.restart.count",
        Unit::Count,
        "Sync rounds that ended, by why they ended and whether the state tip advanced."
    );
    describe_counter!(
        "sync.block.error.count",
        Unit::Count,
        "Block download and verify errors, by kind."
    );
    describe_histogram!(
        "sync.stage.duration_seconds",
        Unit::Seconds,
        "Time taken by each stage of a sync round."
    );
    describe_histogram!(
        "sync.obtain.response.hash.count",
        Unit::Count,
        "Block hashes in each peer response while obtaining tips."
    );
    describe_gauge!(
        "sync.obtain.queued.hash.count",
        Unit::Count,
        "Block hashes queued for download after obtaining tips."
    );
    describe_histogram!(
        "sync.extend.response.hash.count",
        Unit::Count,
        "Block hashes in each peer response while extending tips."
    );
    describe_gauge!(
        "sync.extend.queued.hash.count",
        Unit::Count,
        "Block hashes queued for download after extending tips."
    );
    describe_gauge!(
        "sync.prospective_tips.len",
        Unit::Count,
        "Chain tips the syncer is following."
    );
    describe_gauge!(
        "sync.downloads.in_flight",
        Unit::Count,
        "Blocks the syncer is downloading or verifying."
    );
    describe_gauge!(
        "sync.downloads.oldest_age_seconds",
        Unit::Seconds,
        "How long the oldest block still being downloaded or verified has waited, or 0 when none are."
    );
    describe_gauge!(
        "sync.estimated_network_tip_height",
        Unit::Count,
        "Estimated height of the network's chain tip."
    );
    describe_gauge!(
        "sync.estimated_distance_to_tip",
        Unit::Count,
        "Estimated number of blocks between the state tip and the network's chain tip."
    );
    describe_histogram!(
        "zcash.chain.tip.block_delay_seconds",
        Unit::Seconds,
        "Time from a block's header time until it became this node's best chain tip."
    );

    // Block downloads
    describe_histogram!(
        "sync.block.download.duration_seconds",
        Unit::Seconds,
        "Time taken to download a block during sync."
    );
    describe_histogram!(
        "sync.block.verify.duration_seconds",
        Unit::Seconds,
        "Time taken to verify a downloaded block during sync."
    );
    describe_counter!(
        "sync.downloaded.block.count",
        Unit::Count,
        "Blocks downloaded by the syncer."
    );
    describe_counter!(
        "sync.verified.block.count",
        Unit::Count,
        "Blocks verified after the syncer downloaded them."
    );
    describe_counter!(
        "sync.already.queued.dropped.block.hash.count",
        Unit::Count,
        "Block hashes the syncer ignored because they were already queued for download."
    );
    describe_counter!(
        "sync.no.height.dropped.block.count",
        Unit::Count,
        "Downloaded blocks dropped because they have no coinbase height."
    );
    describe_counter!(
        "sync.cancelled.download.count",
        Unit::Count,
        "Block downloads cancelled by a sync restart."
    );
    describe_counter!(
        "sync.cancelled.verify.ready.count",
        Unit::Count,
        "Block verifications cancelled while waiting for the verifier to be ready."
    );
    describe_counter!(
        "sync.cancelled.verify.count",
        Unit::Count,
        "Block verifications cancelled by a sync restart."
    );
    describe_counter!(
        "sync.max.height.limit.paused.count",
        Unit::Count,
        "Times downloads paused because the syncer got too far ahead of the state tip."
    );
    describe_counter!(
        "sync.max.height.limit.reset.count",
        Unit::Count,
        "Times downloads resumed after the state tip caught up with the syncer."
    );
    describe_counter!(
        "sync.max.height.limit.reset.attempt.count",
        Unit::Count,
        "Attempts to resume downloads after the state tip caught up with the syncer."
    );
}

/// Describes the metrics of blocks gossiped by peers.
fn describe_gossip() {
    describe_gauge!(
        "gossip.queued.block.count",
        Unit::Count,
        "Gossiped blocks queued for download and verification."
    );
    describe_counter!(
        "gossip.already.queued.dropped.block.hash.count",
        Unit::Count,
        "Gossiped block hashes ignored because they were already queued."
    );
    describe_counter!(
        "gossip.full.queue.dropped.block.hash.count",
        Unit::Count,
        "Gossiped block hashes ignored because the download queue was full."
    );
    describe_counter!(
        "gossip.peer.limit.dropped.block.hash.count",
        Unit::Count,
        "Gossiped block hashes ignored because the peer already had a download in progress."
    );
    describe_counter!(
        "gossip.downloaded.block.count",
        Unit::Count,
        "Gossiped blocks downloaded."
    );
    describe_counter!(
        "gossip.verified.block.count",
        Unit::Count,
        "Gossiped blocks verified."
    );
    describe_counter!(
        "gossip.cancelled.count",
        Unit::Count,
        "Gossiped block downloads or verifications cancelled."
    );
    describe_counter!(
        "gossip.no.height.dropped.block.count",
        Unit::Count,
        "Gossiped blocks dropped because they have no coinbase height."
    );
    describe_counter!(
        "gossip.max.height.limit.dropped.block.count",
        Unit::Count,
        "Gossiped blocks dropped because they are too far above the state tip."
    );
    describe_counter!(
        "gossip.min.height.limit.dropped.block.count",
        Unit::Count,
        "Downloaded blocks dropped because they are behind the finalized tip."
    );
}

/// Describes the mempool's metrics.
fn describe_mempool() {
    describe_gauge!(
        "mempool.currently.queued.transactions",
        Unit::Count,
        "Transactions queued for mempool download and verification."
    );
    describe_counter!(
        "mempool.queued.transactions.total",
        Unit::Count,
        "Transactions queued for mempool download and verification."
    );
    describe_counter!(
        "mempool.full_queue.per_peer.total",
        Unit::Count,
        "Transactions ignored because their peer already had too many queued."
    );
    describe_counter!(
        "mempool.downloaded.transactions.total",
        Unit::Count,
        "Transactions downloaded for the mempool, by transaction version."
    );
    describe_counter!(
        "mempool.pushed.transactions.total",
        Unit::Count,
        "Transactions submitted to the mempool directly, such as by RPC, by transaction version."
    );
    describe_counter!(
        "mempool.verified.transactions.total",
        Unit::Count,
        "Transactions verified for the mempool, by transaction version."
    );
    describe_counter!(
        "mempool.failed.verify.tasks.total",
        Unit::Count,
        "Mempool download and verify tasks that failed, by reason."
    );
    describe_counter!(
        "mempool.cancelled.verify.tasks.total",
        Unit::Count,
        "Mempool download and verify tasks cancelled."
    );
    describe_counter!(
        "mempool.gossiped.transactions.total",
        Unit::Count,
        "Mempool transaction IDs gossiped to peers."
    );
    describe_gauge!(
        "mempool.rejected.transaction.ids",
        Unit::Count,
        "Transaction IDs in the mempool's rejected lists."
    );
    describe_gauge!(
        "mempool.rejected.transaction.ids.bytes",
        Unit::Bytes,
        "Approximate memory used by the mempool's rejected lists."
    );
    describe_gauge!(
        "zcash.mempool.size.transactions",
        Unit::Count,
        "Transactions in the mempool."
    );
    describe_gauge!(
        "zcash.mempool.size.bytes",
        Unit::Bytes,
        "Serialized size of the transactions in the mempool."
    );
    describe_gauge!(
        "zcash.mempool.cost.bytes",
        Unit::Bytes,
        "Total ZIP-401 eviction cost of the transactions in the mempool."
    );
    describe_gauge!(
        "zcash.mempool.size.weighted",
        Unit::Bytes,
        "Serialized size of the mempool transactions in each fee weight ratio bucket."
    );
    describe_gauge!(
        "zcash.mempool.actions.unpaid",
        Unit::Count,
        "Unpaid actions in the mempool in each fee weight ratio bucket."
    );
    describe_gauge!(
        "zcash.mempool.actions.paid",
        Unit::Count,
        "Paid actions in the mempool."
    );
}
