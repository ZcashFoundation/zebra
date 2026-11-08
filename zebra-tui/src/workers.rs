//! Background threads that poll the node and the log file.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    app::Update,
    logs::LogTail,
    mempool::{summarize_mempool, MempoolSummary},
    rpc::{NodeRpc, RpcError},
    summary::summarize,
    upgrades::upgrade_schedule,
};

/// How often a waiting poller checks whether the peer list was requested.
const WAKE_INTERVAL: Duration = Duration::from_millis(100);

/// Polls the node's tip and mempool every `interval`, and sends the newest block's statistics when the tip changes.
///
/// While `peers_wanted` is set, it also sends the peer list at most every `interval`.
/// It sends the first peer list as soon as `peers_wanted` becomes set.
/// The thread exits when the receiver is dropped.
pub fn spawn_node_poller(
    rpc: NodeRpc,
    interval: Duration,
    peers_wanted: Arc<AtomicBool>,
    updates: Sender<Update>,
) {
    thread::spawn(move || {
        let mut shown_block_hash: Option<String> = None;
        let mut peers_sent = false;
        loop {
            let mut batch = poll_tip(&rpc, &mut shown_block_hash)
                .unwrap_or_else(|error| vec![Update::RpcFailed(error.to_string())]);
            batch.push(
                poll_mempool(&rpc)
                    .map(Update::Mempool)
                    .unwrap_or_else(|error| Update::RpcFailed(error.to_string())),
            );
            if peers_wanted.load(Ordering::Relaxed) {
                batch.push(
                    rpc.peer_info()
                        .map(Update::Peers)
                        .unwrap_or_else(|error| Update::RpcFailed(error.to_string())),
                );
                peers_sent = true;
            }
            if batch
                .into_iter()
                .any(|update| updates.send(update).is_err())
            {
                return;
            }

            let deadline = Instant::now() + interval;
            while Instant::now() < deadline {
                let wanted = peers_wanted.load(Ordering::Relaxed);
                if wanted && !peers_sent {
                    break;
                }
                if !wanted {
                    peers_sent = false;
                }
                thread::sleep(WAKE_INTERVAL);
            }
        }
    });
}

/// Returns the tip update, and the block statistics if the tip differs from `shown_block_hash`.
fn poll_tip(rpc: &NodeRpc, shown_block_hash: &mut Option<String>) -> Result<Vec<Update>, RpcError> {
    let info = rpc.blockchain_info()?;
    let tip = Update::Tip {
        height: info.blocks,
        estimated_height: info.estimatedheight,
        upgrades: upgrade_schedule(&info),
    };
    if shown_block_hash.as_deref() == Some(info.bestblockhash.as_str()) {
        return Ok(vec![tip]);
    }
    let block = rpc.block(&info.bestblockhash)?;
    *shown_block_hash = Some(info.bestblockhash);
    Ok(vec![tip, Update::Block(Box::new(summarize(&block)))])
}

/// Returns the mempool statistics. It requests the transaction list only when the mempool is not empty.
fn poll_mempool(rpc: &NodeRpc) -> Result<MempoolSummary, RpcError> {
    let info = rpc.mempool_info()?;
    let entries = if info.size > 0 {
        rpc.mempool_entries()?
    } else {
        HashMap::new()
    };
    Ok(summarize_mempool(&info, entries.values()))
}

/// Sends lines appended to the log file every `interval`. The thread exits when the receiver is dropped.
pub fn spawn_log_follower(mut tail: LogTail, interval: Duration, updates: Sender<Update>) {
    thread::spawn(move || loop {
        let update = match tail.poll() {
            Ok(lines) if lines.is_empty() => None,
            Ok(lines) => Some(Update::LogLines(lines)),
            Err(error) => Some(Update::LogFailed(error.to_string())),
        };
        if update.is_some_and(|update| updates.send(update).is_err()) {
            return;
        }
        thread::sleep(interval);
    });
}
