use std::time::Duration;

use serde_json::{Map, Number, Value};
use zebra_chain::block;
use zebra_network::zakura::{
    commit_state_trace as cs_trace, zakura_trace_peer_label, BlockApplyResult, ZakuraPeerId,
    ZakuraTrace, COMMIT_STATE_TABLE,
};

pub(crate) mod block_sync_driver;
pub(crate) mod frontier;
pub(crate) mod header_sync_driver;
pub(crate) mod throughput_probe;

pub(crate) use block_sync_driver::drive_block_sync_actions;
#[cfg(test)]
pub(crate) use block_sync_driver::{
    apply_block_sync_body, block_apply_class, block_sync_missing_body_window,
    block_sync_needed_blocks_from_state, coalesce_ready_needed_block_queries,
    coalesce_stale_needed_block_queries, commit_block_sync_body, query_block_sync_needed_blocks,
    BlockApplyClass, ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL,
    ZAKURA_BLOCK_SYNC_MISSING_BODY_WINDOW,
};
pub(crate) use frontier::{query_block_sync_frontiers, verified_block_tip_from_state};
#[cfg(test)]
pub(crate) use header_sync_driver::{
    block_sync_chain_tip_event, body_sizes_for_served_header_range,
    chain_tip_mirror_frontier_change, header_range_commit_failure_kind,
    notify_block_sync_header_tip,
};
pub(crate) use header_sync_driver::{
    drive_zakura_header_sync_actions, mirror_zakura_full_block_commits,
    zakura_header_sync_driver_startup, ZakuraHeaderSyncDriverHandles,
};
pub(crate) use throughput_probe::{BlocksyncThroughputProbe, BlocksyncThroughputSummary};

pub(crate) const ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn emit_commit_state(
    trace: &ZakuraTrace,
    event: &'static str,
    source: &'static str,
    build: impl FnOnce(&mut Map<String, Value>),
) {
    trace.emit_with(COMMIT_STATE_TABLE, |row| {
        row.insert(
            cs_trace::EVENT.to_string(),
            Value::String(event.to_string()),
        );
        insert_cs_str(row, cs_trace::SOURCE, source);
        build(row);
    });
}

pub(crate) fn insert_cs_height(
    row: &mut Map<String, Value>,
    key: &'static str,
    height: block::Height,
) {
    insert_cs_u64(row, key, u64::from(height.0));
}

pub(crate) fn insert_cs_hash(row: &mut Map<String, Value>, key: &'static str, hash: block::Hash) {
    row.insert(key.to_string(), Value::String(format!("{hash}")));
}

pub(crate) fn insert_cs_peer(row: &mut Map<String, Value>, key: &'static str, peer: &ZakuraPeerId) {
    row.insert(
        key.to_string(),
        Value::String(zakura_trace_peer_label(peer)),
    );
}

pub(crate) fn insert_cs_u64(row: &mut Map<String, Value>, key: &'static str, value: u64) {
    row.insert(key.to_string(), Value::Number(Number::from(value)));
}

pub(crate) fn insert_cs_bool(row: &mut Map<String, Value>, key: &'static str, value: bool) {
    row.insert(key.to_string(), Value::Bool(value));
}

pub(crate) fn insert_cs_str(row: &mut Map<String, Value>, key: &'static str, value: &str) {
    row.insert(key.to_string(), Value::String(value.to_string()));
}

pub(crate) fn insert_cs_frontiers(
    row: &mut Map<String, Value>,
    frontiers: &zebra_network::zakura::BlockSyncFrontiers,
) {
    insert_cs_height(row, cs_trace::FINALIZED_HEIGHT, frontiers.finalized_height);
    insert_cs_height(
        row,
        cs_trace::VERIFIED_BLOCK_TIP,
        frontiers.verified_block_tip,
    );
    insert_cs_hash(
        row,
        cs_trace::VERIFIED_BLOCK_HASH,
        frontiers.verified_block_hash,
    );
}

pub(crate) fn block_apply_result_label(result: BlockApplyResult) -> &'static str {
    match result {
        BlockApplyResult::Committed => "committed",
        BlockApplyResult::Duplicate => "duplicate",
        BlockApplyResult::Rejected => "rejected",
        BlockApplyResult::TimedOut => "timed_out",
    }
}

pub(crate) fn block_verify_error_is_duplicate<Error>(error: &Error) -> bool
where
    Error: std::fmt::Debug + Send + Sync + 'static,
{
    let error = error as &dyn std::any::Any;

    error
        .downcast_ref::<zebra_consensus::RouterError>()
        .is_some_and(zebra_consensus::RouterError::is_duplicate_request)
        || error
            .downcast_ref::<zebra_consensus::VerifyBlockError>()
            .is_some_and(zebra_consensus::VerifyBlockError::is_duplicate_request)
        || error
            .downcast_ref::<zebra_consensus::BoxError>()
            .is_some_and(|error| {
                error
                    .downcast_ref::<zebra_consensus::RouterError>()
                    .is_some_and(zebra_consensus::RouterError::is_duplicate_request)
                    || error
                        .downcast_ref::<zebra_consensus::VerifyBlockError>()
                        .is_some_and(zebra_consensus::VerifyBlockError::is_duplicate_request)
            })
}
