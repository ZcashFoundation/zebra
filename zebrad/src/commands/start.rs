//! `start` subcommand - entry point for starting a zebra node
//!
//! ## Application Structure
//!
//! A zebra node consists of the following major services and tasks:
//!
//! Peers:
//!  * Peer Connection Pool Service
//!    * primary external interface for outbound requests from this node to remote peers
//!    * accepts requests from services and tasks in this node, and sends them to remote peers
//!  * Peer Discovery Service
//!    * maintains a list of peer addresses, and connection priority metadata
//!    * discovers new peer addresses from existing peer connections
//!    * initiates new outbound peer connections in response to demand from tasks within this node
//!  * Peer Cache Service
//!    * Reads previous peer cache on startup, and adds it to the configured DNS seed peers
//!    * Periodically updates the peer cache on disk from the latest address book state
//!
//! Blocks & Mempool Transactions:
//!  * Consensus Service
//!    * handles all validation logic for the node
//!    * verifies blocks using zebra-chain, then stores verified blocks in zebra-state
//!    * verifies mempool and block transactions using zebra-chain and zebra-script,
//!      and returns verified mempool transactions for mempool storage
//!  * Inbound Service
//!    * primary external interface for inbound peer requests to this node
//!    * handles requests from peers for network data, chain data, and mempool transactions
//!    * spawns download and verify tasks for each gossiped block
//!    * sends gossiped transactions to the mempool service
//!
//! Blocks:
//!  * Sync Task
//!    * runs in the background and continuously queries the network for
//!      new blocks to be verified and added to the local state
//!    * spawns download and verify tasks for each crawled block
//!  * State Service
//!    * contextually verifies blocks
//!    * handles in-memory storage of multiple non-finalized chains
//!    * handles permanent storage of the best finalized chain
//!  * Old State Version Cleanup Task
//!    * deletes outdated state versions
//!  * Block Gossip Task
//!    * runs in the background and continuously queries the state for
//!      newly committed blocks to be gossiped to peers
//!  * Progress Task
//!    * logs progress towards the chain tip
//!
//! Block Mining:
//!  * Internal Miner Task
//!    * if the user has configured Zebra to mine blocks, spawns tasks to generate new blocks,
//!      and submits them for verification. This automatically shares these new blocks with peers.
//!
//! Mempool Transactions:
//!  * Mempool Service
//!    * activates when the syncer is near the chain tip
//!    * spawns download and verify tasks for each crawled or gossiped transaction
//!    * handles in-memory storage of unmined transactions
//!  * Queue Checker Task
//!    * runs in the background, polling the mempool to store newly verified transactions
//!  * Transaction Gossip Task
//!    * runs in the background and gossips newly added mempool transactions
//!      to peers
//!
//! Remote Procedure Calls:
//!  * JSON-RPC Service
//!    * answers RPC client requests using the State Service and Mempool Service
//!    * submits client transactions to the node's mempool
//!
//! Zebra also has diagnostic support:
//! * [metrics](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/metrics.md)
//! * [tracing](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/tracing.md)
//! * [progress-bar](https://docs.rs/howudoin/0.1.1/howudoin)
//!
//! Some of the diagnostic features are optional, and need to be enabled at compile-time.

use std::{
    collections::HashMap, future::Future, net::SocketAddr, path::Path, sync::Arc, time::Duration,
};

use abscissa_core::{config, Command, FrameworkError};
use color_eyre::eyre::{eyre, Report};
use futures::FutureExt;
use tokio::{
    pin, select,
    sync::{mpsc, oneshot, watch},
};
use tower::{builder::ServiceBuilder, util::BoxService, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::block::{self, genesis::regtest_genesis_block};
use zebra_consensus::router::BackgroundTaskHandles;
use zebra_network::types::PeerServices;
use zebra_network::zakura::{
    BlockSizeEstimate, BlockSyncAction, BlockSyncBlockMeta, BlockSyncEvent, BlockSyncFrontiers,
    BlockSyncHandle, BlockSyncMisbehavior, HeaderSyncAction, HeaderSyncCommitFailureKind,
    HeaderSyncEvent, HeaderSyncFrontiers, ZakuraEndpoint, ZakuraHeaderSyncDriverStartup,
    DEFAULT_HS_RANGE,
};
use zebra_rpc::{methods::RpcImpl, server::RpcServer, SubmitBlockChannel};

use crate::{
    application::{build_version, user_agent, LAST_WARN_ERROR_LOG_SENDER},
    components::{
        health,
        inbound::{self, InboundSetupData, MAX_INBOUND_RESPONSE_TIME},
        mempool::{self, Mempool},
        sync::{self, show_block_chain_progress, VERIFICATION_PIPELINE_SCALING_MULTIPLIER},
        tokio::{RuntimeRun, TokioComponent},
        zcashd_compat, ChainSync, Inbound,
    },
    config::ZebradConfig,
    prelude::*,
};

#[cfg(feature = "internal-miner")]
use crate::components;

/// Start the application (default command)
#[derive(Command, Debug, Default, clap::Parser)]
pub struct StartCmd {
    /// Filter strings which override the config file and defaults
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,

    /// Enable zcashd-compat mode and apply zcashd-compat RPC guardrails.
    #[clap(long)]
    zcashd_compat: bool,

    /// Continue startup even when zcashd-compat preflight detects minimum hardware shortfalls.
    #[clap(long = "unsafe-low-specs")]
    unsafe_low_specs: bool,
}

/// Warns if Linux TCP slow-start-after-idle is enabled, which significantly
/// reduces single-peer throughput for block propagation.
///
/// See `book/src/user/troubleshooting.md`.
#[cfg(target_os = "linux")]
fn check_tcp_slow_start_after_idle() {
    const PATH: &str = "/proc/sys/net/ipv4/tcp_slow_start_after_idle";

    let raw = match std::fs::read_to_string(PATH) {
        Ok(raw) => raw,
        Err(error) => {
            debug!(
                ?error,
                path = PATH,
                "could not read TCP sysctl, skipping check"
            );
            return;
        }
    };

    if raw.trim() == "0" {
        return;
    }

    warn!(
        setting = "net.ipv4.tcp_slow_start_after_idle",
        "TCP slow-start-after-idle is enabled, which resets TCP's congestion window \
         between block requests and significantly reduces single-peer throughput for \
         block propagation. \
         Hint: set `net.ipv4.tcp_slow_start_after_idle=0` via sysctl. \
         See https://zebra.zfnd.org/user/troubleshooting.html#linux-tcp-tuning-for-block-propagation"
    );
}

async fn zakura_header_sync_driver_startup(
    read_state: zebra_state::ReadStateService,
    network: &zebra_chain::parameters::Network,
) -> Result<ZakuraHeaderSyncDriverStartup, Report> {
    let best_header_tip = match read_state
        .clone()
        .oneshot(zebra_state::ReadRequest::BestHeaderTip)
        .await
        .map_err(|error| eyre!("{error}"))?
    {
        zebra_state::ReadResponse::BestHeaderTip(tip) => tip,
        response => Err(eyre!("unexpected BestHeaderTip response: {response:?}"))?,
    };

    let finalized_tip = match read_state
        .clone()
        .oneshot(zebra_state::ReadRequest::FinalizedTip)
        .await
        .map_err(|error| eyre!("{error}"))?
    {
        zebra_state::ReadResponse::FinalizedTip(tip) => tip,
        response => Err(eyre!("unexpected FinalizedTip response: {response:?}"))?,
    };

    let verified_block_tip = match read_state
        .oneshot(zebra_state::ReadRequest::Tip)
        .await
        .map_err(|error| eyre!("{error}"))?
    {
        zebra_state::ReadResponse::Tip(tip) => tip,
        response => Err(eyre!("unexpected Tip response: {response:?}"))?,
    };

    let empty_state_tip = (block::Height(0), network.genesis_hash());
    let verified_block_tip = verified_block_tip.unwrap_or(empty_state_tip);
    Ok(ZakuraHeaderSyncDriverStartup {
        frontiers: HeaderSyncFrontiers {
            finalized_height: finalized_tip.map_or(block::Height(0), |(height, _)| height),
            verified_block_tip: verified_block_tip.0,
        },
        best_header_tip: Some(best_header_tip.unwrap_or(empty_state_tip)),
        verified_block_tip_hash: verified_block_tip.1,
    })
}

#[derive(Clone)]
struct ZakuraHeaderSyncDriverHandles {
    endpoint: ZakuraEndpoint,
    header_sync: zebra_network::zakura::HeaderSyncHandle,
    block_sync: Option<BlockSyncHandle>,
}

async fn drive_zakura_header_sync_actions<State, ReadState, BlockVerifier>(
    mut actions: mpsc::Receiver<HeaderSyncAction>,
    handles: ZakuraHeaderSyncDriverHandles,
    state: State,
    read_state: ReadState,
    block_verifier: BlockVerifier,
    shutdown: impl Future<Output = ()> + Send + 'static,
) where
    State: Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    State::Future: Send + 'static,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    pin!(shutdown);
    loop {
        let action = select! {
            _ = &mut shutdown => return,
            action = actions.recv() => {
                let Some(action) = action else {
                    return;
                };
                action
            }
        };

        match action {
            HeaderSyncAction::Misbehavior { peer, reason } => {
                debug!(
                    ?peer,
                    ?reason,
                    "disconnecting peer for Zakura header-sync violation"
                );
                let _ = handles.endpoint.supervisor().disconnect_peer(&peer).await;
            }
            HeaderSyncAction::NewBlockReceived {
                peer,
                height,
                hash,
                block,
            } => {
                match block_verifier
                    .clone()
                    .oneshot(zebra_consensus::Request::Commit(block.clone()))
                    .await
                {
                    Ok(committed_hash) if committed_hash == hash => {
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::NewBlockAccepted {
                                peer,
                                height,
                                hash,
                                block,
                            })
                            .await;
                    }
                    Ok(committed_hash) => {
                        warn!(
                            ?peer,
                            ?hash,
                            ?committed_hash,
                            "Zakura NewBlock verifier returned an unexpected hash"
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::NewBlockRejected { peer, hash })
                            .await;
                    }
                    Err(error) => {
                        if block_verify_error_is_duplicate(&error) {
                            debug!(
                                ?peer,
                                ?height,
                                ?hash,
                                ?error,
                                "Zakura NewBlock was already known by the block verifier"
                            );
                            let _ = handles
                                .header_sync
                                .send(HeaderSyncEvent::NewBlockDuplicate { peer, height, hash })
                                .await;
                            continue;
                        }

                        debug!(
                            ?peer,
                            ?hash,
                            ?error,
                            "Zakura NewBlock rejected by block verifier"
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::NewBlockRejected { peer, hash })
                            .await;
                    }
                }
            }
            HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
                match read_state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::HeadersByHeightRange { start, count })
                    .await
                {
                    Ok(zebra_state::ReadResponse::Headers(headers)) => {
                        let body_size_hints = match read_state
                            .clone()
                            .oneshot(zebra_state::ReadRequest::BlockSizeHints {
                                from: start,
                                count,
                            })
                            .await
                        {
                            Ok(zebra_state::ReadResponse::BlockSizeHints(hints)) => hints,
                            Ok(response) => {
                                warn!(?peer, ?response, "unexpected BlockSizeHints response");
                                Vec::new()
                            }
                            Err(error) => {
                                warn!(
                                    ?peer,
                                    ?error,
                                    "failed to read Zakura BlockSizeHints response from state"
                                );
                                Vec::new()
                            }
                        };
                        let body_sizes = body_sizes_for_served_header_range(
                            start,
                            headers.iter().map(|(height, _, _)| *height),
                            &body_size_hints,
                        );
                        let headers = headers
                            .into_iter()
                            .map(|(_height, _hash, header)| header)
                            .collect();
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeResponseReady {
                                peer,
                                start_height: start,
                                requested_count: count,
                                headers,
                                body_sizes,
                            })
                            .await;
                    }
                    Ok(response) => {
                        warn!(?peer, ?response, "unexpected HeadersByHeightRange response");
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                    Err(error) => {
                        warn!(
                            ?peer,
                            ?error,
                            "failed to read Zakura Headers response from state"
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                }
            }
            HeaderSyncAction::CommitHeaderRange {
                peer,
                anchor,
                start_height,
                headers,
                body_sizes,
                finalized: _finalized,
            } => {
                let count = u32::try_from(headers.len()).unwrap_or(u32::MAX);
                match state
                    .clone()
                    .oneshot(zebra_state::Request::CommitHeaderRange {
                        anchor,
                        headers,
                        body_sizes,
                    })
                    .await
                {
                    Ok(zebra_state::Response::Committed(tip_hash)) => {
                        let tip_height =
                            block::Height(start_height.0.saturating_add(count.saturating_sub(1)));
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitted {
                                start_height,
                                tip_height,
                                tip_hash,
                            })
                            .await;
                        notify_block_sync_header_tip(
                            handles.block_sync.as_ref(),
                            tip_height,
                            tip_hash,
                        )
                        .await;
                    }
                    Ok(response) => {
                        warn!(?peer, ?response, "unexpected CommitHeaderRange response");
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitFailed {
                                peer,
                                start_height,
                                count,
                                kind: HeaderSyncCommitFailureKind::Local,
                            })
                            .await;
                    }
                    Err(error) => {
                        let kind = header_range_commit_failure_kind(error.as_ref());
                        debug!(
                            ?peer,
                            ?start_height,
                            ?count,
                            ?kind,
                            ?error,
                            "Zakura header range commit failed"
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitFailed {
                                peer,
                                start_height,
                                count,
                                kind,
                            })
                            .await;
                    }
                }
            }
            HeaderSyncAction::QueryBestHeaderTip => {
                match read_state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::BestHeaderTip)
                    .await
                {
                    Ok(zebra_state::ReadResponse::BestHeaderTip(Some((tip_height, tip_hash)))) => {
                        // Reuse the range-commit event as a startup/tip-refresh fact: a
                        // single-height covered mark is harmless and refreshes the reactor tip.
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitted {
                                start_height: tip_height,
                                tip_height,
                                tip_hash,
                            })
                            .await;
                        notify_block_sync_header_tip(
                            handles.block_sync.as_ref(),
                            tip_height,
                            tip_hash,
                        )
                        .await;
                    }
                    Ok(zebra_state::ReadResponse::BestHeaderTip(None)) => {}
                    Ok(response) => warn!(?response, "unexpected BestHeaderTip response"),
                    Err(error) => warn!(?error, "failed to query Zakura best header tip"),
                }
            }
            HeaderSyncAction::QueryMissingBlockBodies { from, limit } => {
                log_missing_block_bodies(read_state.clone(), from, limit).await;
            }
            HeaderSyncAction::BodyGaps { from, to } => {
                let limit =
                    to.0.saturating_sub(from.0)
                        .saturating_add(1)
                        .min(DEFAULT_HS_RANGE);
                log_missing_block_bodies(read_state.clone(), from, limit).await;
            }
        }
    }
}

async fn notify_block_sync_header_tip(
    block_sync: Option<&BlockSyncHandle>,
    height: block::Height,
    hash: block::Hash,
) {
    if let Some(block_sync) = block_sync {
        let _ = block_sync
            .send(BlockSyncEvent::HeaderTipChanged { height, hash })
            .await;
    }
}

const ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT: Duration = Duration::from_secs(30);

async fn drive_block_sync_actions<ReadState, BlockVerifier>(
    mut actions: mpsc::Receiver<BlockSyncAction>,
    supervisor: zebra_network::zakura::ZakuraSupervisorHandle,
    block_sync: BlockSyncHandle,
    read_state: ReadState,
    block_verifier: BlockVerifier,
    shutdown: impl Future<Output = ()> + Send + 'static,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    pin!(shutdown);
    loop {
        let action = select! {
            _ = &mut shutdown => return,
            action = actions.recv() => {
                let Some(action) = action else {
                    return;
                };
                action
            }
        };

        match action {
            BlockSyncAction::SendMessage { .. } => {
                // The B2 reactor already writes stream-6 frames through
                // BlockSyncPeerSession. Forwarding this action here would double-send.
            }
            BlockSyncAction::Misbehavior { peer, reason } => {
                if block_sync_misbehavior_is_hard(reason) {
                    debug!(
                        ?peer,
                        ?reason,
                        "disconnecting peer for Zakura block-sync violation"
                    );
                    let _ = supervisor.disconnect_peer(&peer).await;
                } else {
                    debug!(
                        ?peer,
                        ?reason,
                        "recorded soft Zakura block-sync peer violation"
                    );
                }
            }
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                match query_block_sync_needed_blocks(
                    read_state.clone(),
                    verified_block_tip,
                    best_header_tip,
                )
                .await
                {
                    Ok(blocks) => {
                        let _ = block_sync.send(BlockSyncEvent::NeededBlocks(blocks)).await;
                    }
                    Err(error) => {
                        warn!(
                            ?verified_block_tip,
                            ?best_header_tip,
                            ?error,
                            "failed to query Zakura block-sync needed blocks"
                        );
                    }
                }
            }
            BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                match tokio::time::timeout(
                    ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
                    read_state
                        .clone()
                        .oneshot(zebra_state::ReadRequest::BlocksByHeightRange { start, count }),
                )
                .await
                {
                    Ok(Ok(zebra_state::ReadResponse::Blocks(blocks))) => {
                        let _ = block_sync
                            .send(BlockSyncEvent::BlockRangeResponseReady {
                                peer,
                                start_height: start,
                                requested_count: count,
                                blocks,
                            })
                            .await;
                    }
                    Ok(Ok(response)) => {
                        warn!(?peer, ?response, "unexpected BlocksByHeightRange response");
                        let _ = block_sync
                            .send(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                    Ok(Err(error)) => {
                        warn!(
                            ?peer,
                            ?error,
                            "failed to read Zakura Blocks response from state"
                        );
                        let _ = block_sync
                            .send(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                    Err(_elapsed) => {
                        warn!(?peer, "timed out reading Zakura block-sync serving range");
                        let _ = block_sync
                            .send(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                }
            }
            BlockSyncAction::SubmitBlock { block } => {
                commit_block_sync_body(
                    block_verifier.clone(),
                    read_state.clone(),
                    &block_sync,
                    block,
                )
                .await;
            }
        }
    }
}

fn block_sync_misbehavior_is_hard(reason: BlockSyncMisbehavior) -> bool {
    matches!(
        reason,
        BlockSyncMisbehavior::MalformedMessage
            | BlockSyncMisbehavior::UnsolicitedBlock
            | BlockSyncMisbehavior::GetBlocksTooLong
            | BlockSyncMisbehavior::InvalidBlock
            | BlockSyncMisbehavior::InvalidStatus
            | BlockSyncMisbehavior::UnsolicitedDone
            | BlockSyncMisbehavior::StatusSpam
    )
}

async fn commit_block_sync_body<BlockVerifier, ReadState>(
    block_verifier: BlockVerifier,
    read_state: ReadState,
    block_sync: &BlockSyncHandle,
    block: Arc<block::Block>,
) where
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let expected_hash = block.hash();
    let height = block.coinbase_height();
    match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        block_verifier
            .clone()
            .oneshot(zebra_consensus::Request::Commit(block)),
    )
    .await
    {
        Ok(Ok(committed_hash)) if committed_hash == expected_hash => {
            debug!(
                ?height,
                ?committed_hash,
                "Zakura block sync committed block body through verifier"
            );
        }
        Ok(Ok(committed_hash)) => {
            warn!(
                ?height,
                ?expected_hash,
                ?committed_hash,
                "Zakura block-sync verifier returned an unexpected hash"
            );
            refresh_block_sync_frontiers(read_state, block_sync).await;
        }
        Ok(Err(error)) => {
            if block_verify_error_is_duplicate(&error) {
                debug!(
                    ?height,
                    ?expected_hash,
                    ?error,
                    "Zakura block-sync body was already known by the block verifier"
                );
            } else {
                debug!(
                    ?height,
                    ?expected_hash,
                    ?error,
                    "Zakura block-sync body rejected by block verifier"
                );
            }
            refresh_block_sync_frontiers(read_state, block_sync).await;
        }
        Err(_elapsed) => {
            warn!(
                ?height,
                ?expected_hash,
                "timed out committing Zakura block-sync body"
            );
            refresh_block_sync_frontiers(read_state, block_sync).await;
        }
    }
}

async fn refresh_block_sync_frontiers<ReadState>(
    read_state: ReadState,
    block_sync: &BlockSyncHandle,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let finalized_height = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::FinalizedTip),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::FinalizedTip(Some((height, _hash))))) => height,
        Ok(Ok(zebra_state::ReadResponse::FinalizedTip(None))) => block::Height(0),
        Ok(Ok(response)) => {
            warn!(?response, "unexpected FinalizedTip response");
            block::Height(0)
        }
        Ok(Err(error)) => {
            warn!(
                ?error,
                "failed to refresh Zakura block-sync finalized frontier"
            );
            block::Height(0)
        }
        Err(_elapsed) => {
            warn!("timed out refreshing Zakura block-sync finalized frontier");
            block::Height(0)
        }
    };

    match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state.oneshot(zebra_state::ReadRequest::Tip),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::Tip(Some((height, hash))))) => {
            let _ = block_sync
                .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
                    finalized_height,
                    verified_block_tip: height,
                    verified_block_hash: hash,
                }))
                .await;
        }
        Ok(Ok(zebra_state::ReadResponse::Tip(None))) => {}
        Ok(Ok(response)) => warn!(?response, "unexpected Tip response"),
        Ok(Err(error)) => warn!(?error, "failed to refresh Zakura block-sync body frontier"),
        Err(_elapsed) => warn!("timed out refreshing Zakura block-sync body frontier"),
    }
}

async fn query_block_sync_needed_blocks<ReadState>(
    read_state: ReadState,
    verified_block_tip: block::Height,
    best_header_tip: block::Height,
) -> Result<Vec<BlockSyncBlockMeta>, zebra_state::BoxError>
where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let Some((from, limit)) = block_sync_missing_body_window(verified_block_tip, best_header_tip)
    else {
        return Ok(Vec::new());
    };

    let missing = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::MissingBlockBodies { from, limit }),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::MissingBlockBodies(heights))) => heights,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected MissingBlockBodies response");
            return Ok(Vec::new());
        }
        Ok(Err(error)) => return Err(error),
        Err(elapsed) => return Err(Box::new(elapsed)),
    };

    let Some(first) = missing.first().copied() else {
        return Ok(Vec::new());
    };
    let Some(last) = missing.last().copied() else {
        return Ok(Vec::new());
    };
    let span = last.0.saturating_sub(first.0).saturating_add(1);

    let headers = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::HeadersByHeightRange {
                start: first,
                count: span,
            }),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::Headers(headers))) => headers,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected HeadersByHeightRange response");
            return Ok(Vec::new());
        }
        Ok(Err(error)) => return Err(error),
        Err(elapsed) => return Err(Box::new(elapsed)),
    };

    let size_hints = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state.oneshot(zebra_state::ReadRequest::BlockSizeHints {
            from: first,
            count: span,
        }),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::BlockSizeHints(hints))) => hints,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected BlockSizeHints response");
            Vec::new()
        }
        Ok(Err(error)) => return Err(error),
        Err(elapsed) => return Err(Box::new(elapsed)),
    };

    Ok(block_sync_needed_blocks_from_state(
        missing, headers, size_hints,
    ))
}

fn block_sync_missing_body_window(
    verified_block_tip: block::Height,
    best_header_tip: block::Height,
) -> Option<(block::Height, u32)> {
    if best_header_tip <= verified_block_tip {
        return None;
    }

    let from = block::Height(verified_block_tip.0.saturating_add(1));
    let limit = best_header_tip
        .0
        .saturating_sub(verified_block_tip.0)
        .clamp(1, zebra_state::MAX_BLOCK_REORG_HEIGHT);
    Some((from, limit))
}

fn block_sync_needed_blocks_from_state(
    missing: Vec<block::Height>,
    headers: Vec<(block::Height, block::Hash, Arc<block::Header>)>,
    size_hints: Vec<(block::Height, Option<u32>)>,
) -> Vec<BlockSyncBlockMeta> {
    let headers: HashMap<_, _> = headers
        .into_iter()
        .map(|(height, hash, _header)| (height, hash))
        .collect();
    let size_hints: HashMap<_, _> = size_hints.into_iter().collect();

    missing
        .into_iter()
        .filter_map(|height| {
            let hash = *headers.get(&height)?;
            let size = size_hints
                .get(&height)
                .copied()
                .flatten()
                .filter(|size| *size > 0)
                .map(BlockSizeEstimate::Advertised)
                .unwrap_or(BlockSizeEstimate::Unknown);

            Some(BlockSyncBlockMeta { height, hash, size })
        })
        .collect()
}

fn body_sizes_for_served_header_range(
    start: block::Height,
    header_heights: impl IntoIterator<Item = block::Height>,
    body_size_hints: &[(block::Height, Option<u32>)],
) -> Vec<u32> {
    header_heights
        .into_iter()
        .map(|height| {
            let Some(offset) = usize::try_from(height - start).ok() else {
                return 0;
            };

            body_size_hints
                .get(offset)
                .and_then(|(hint_height, size)| {
                    (*hint_height == height).then_some(size.unwrap_or(0))
                })
                .unwrap_or(0)
        })
        .collect()
}

fn block_verify_error_is_duplicate<Error>(error: &Error) -> bool
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

async fn log_missing_block_bodies<ReadState>(read_state: ReadState, from: block::Height, limit: u32)
where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    // This driver boundary is observational only: header sync exposes bounded body gaps
    // from state, but block download must consume them through its own policy/watches.
    match read_state
        .oneshot(zebra_state::ReadRequest::MissingBlockBodies { from, limit })
        .await
    {
        Ok(zebra_state::ReadResponse::MissingBlockBodies(heights)) => {
            let first = heights.first().copied();
            let last = heights.last().copied();
            let count = heights.len();
            debug!(
                ?from,
                ?limit,
                ?count,
                ?first,
                ?last,
                "Zakura header-known body gaps from state"
            );
        }
        Ok(response) => warn!(?response, "unexpected MissingBlockBodies response"),
        Err(error) => warn!(?error, "failed to query Zakura missing block bodies"),
    }
}

fn header_range_commit_failure_kind(
    error: &(dyn std::error::Error + Send + Sync + 'static),
) -> HeaderSyncCommitFailureKind {
    let Some(error) = error.downcast_ref::<zebra_state::CommitHeaderRangeError>() else {
        return HeaderSyncCommitFailureKind::Local;
    };

    match error {
        zebra_state::CommitHeaderRangeError::StorageWriteError { .. }
        | zebra_state::CommitHeaderRangeError::SendCommitRequestFailed
        | zebra_state::CommitHeaderRangeError::CommitResponseDropped => {
            HeaderSyncCommitFailureKind::Local
        }
        zebra_state::CommitHeaderRangeError::EmptyRange
        | zebra_state::CommitHeaderRangeError::RangeTooLong { .. }
        | zebra_state::CommitHeaderRangeError::UnknownAnchor { .. }
        | zebra_state::CommitHeaderRangeError::HeightOverflow
        | zebra_state::CommitHeaderRangeError::ImmutableConflict { .. }
        | zebra_state::CommitHeaderRangeError::ReorgTooDeep { .. }
        | zebra_state::CommitHeaderRangeError::CheckpointConflict { .. }
        | zebra_state::CommitHeaderRangeError::ConflictingFullBlockHeader { .. }
        | zebra_state::CommitHeaderRangeError::ValidateContextError(_) => {
            HeaderSyncCommitFailureKind::InvalidPeerRange
        }
        _ => HeaderSyncCommitFailureKind::Local,
    }
}

async fn mirror_zakura_full_block_commits<ReadState>(
    mut chain_tip_change: zebra_state::ChainTipChange,
    read_state: ReadState,
    header_sync: zebra_network::zakura::HeaderSyncHandle,
    block_sync: Option<BlockSyncHandle>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    pin!(shutdown);
    loop {
        let action = select! {
            _ = &mut shutdown => return,
            action = chain_tip_change.wait_for_tip_change() => {
                let Ok(action) = action else {
                    return;
                };
                action
            }
        };
        let height = action.best_tip_height();
        let hash = action.best_tip_hash();

        let finalized_height = match read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::FinalizedTip)
            .await
        {
            Ok(zebra_state::ReadResponse::FinalizedTip(Some((height, _hash)))) => height,
            Ok(zebra_state::ReadResponse::FinalizedTip(None)) => block::Height(0),
            Ok(response) => {
                warn!(?response, "unexpected FinalizedTip response");
                block::Height(0)
            }
            Err(error) => {
                warn!(?error, "failed to query Zakura finalized frontier");
                block::Height(0)
            }
        };

        let _ = header_sync
            .send(HeaderSyncEvent::StateFrontiersChanged(
                HeaderSyncFrontiers {
                    finalized_height,
                    verified_block_tip: height,
                },
            ))
            .await;
        if let Some(block_sync) = &block_sync {
            let frontiers = BlockSyncFrontiers {
                finalized_height,
                verified_block_tip: height,
                verified_block_hash: hash,
            };
            let _ = block_sync
                .send(block_sync_chain_tip_event(&action, frontiers))
                .await;
        }

        match read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Block(hash.into()))
            .await
        {
            Ok(zebra_state::ReadResponse::Block(Some(block))) => {
                let _ = header_sync
                    .send(HeaderSyncEvent::FullBlockCommitted {
                        height,
                        hash,
                        header: block.header.clone(),
                    })
                    .await;
            }
            Ok(zebra_state::ReadResponse::Block(None)) => {
                debug!(
                    ?height,
                    ?hash,
                    "Zakura full-block mirror could not find committed tip block"
                );
            }
            Ok(response) => warn!(?response, "unexpected block lookup response"),
            Err(error) => warn!(?error, "failed to mirror Zakura full-block commit"),
        }
    }
}

fn block_sync_chain_tip_event(
    action: &zebra_state::TipAction,
    frontiers: BlockSyncFrontiers,
) -> BlockSyncEvent {
    match action {
        zebra_state::TipAction::Grow { .. } => BlockSyncEvent::ChainTipGrow(frontiers),
        zebra_state::TipAction::Reset { .. } => BlockSyncEvent::ChainTipReset(frontiers),
    }
}

fn replace_legacy_syncer_on_zakura_path(config: &zebra_network::Config) -> bool {
    config.v2_p2p && config.zakura.block_sync.replace_legacy_syncer
}

#[cfg(not(target_os = "linux"))]
fn check_tcp_slow_start_after_idle() {}

impl StartCmd {
    /// Minimum response body size used in zcashd-compat mode.
    ///
    /// zcashd defaults to a 128 MiB response budget. That allows a memory-clamped
    /// batch of 33 raw blocks, whose worst-case response is 133,082,368 bytes.
    /// jsonrpsee applies this limit to the whole JSON-RPC batch response.
    ///
    /// These `ZCASHD_COMPAT_*` constants mirror zcashd's batch/budget arithmetic
    /// in `zcash/src/zebra_compat/zebra_client.cpp`
    /// (`ZebraCompatSyncBatchSizeFromMemoryBudget` / `ZebraRpcMaxResponseBodySize`).
    /// If the formula or constants change on either side, update both together,
    /// or startup validation will accept configs the other process rejects.
    const ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE: usize = 128 * 1024 * 1024;
    /// zcashd's default sync batch size.
    const ZCASHD_COMPAT_DEFAULT_SYNC_BATCH_SIZE: u64 = 30;
    /// Default zcashd raw-block sync response budget.
    const ZCASHD_COMPAT_DEFAULT_SYNC_RESPONSE_BUDGET_MB: u64 = 128;
    /// MiB in bytes, matching zcashd's `-zebra-compat-sync-response-budget-mb`.
    const ZCASHD_COMPAT_MIB: u64 = 1024 * 1024;
    /// zcashd's consensus maximum serialized block size.
    const ZCASHD_COMPAT_MAX_BLOCK_BYTES: u64 = 2_000_000;
    /// zcashd's per-block JSON-RPC response overhead allowance.
    const ZCASHD_COMPAT_JSON_RPC_BLOCK_OVERHEAD_BYTES: u64 = 1024;
    /// zcashd's whole-batch JSON-RPC response margin.
    const ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES: u64 = 1024 * 1024;
    /// Conservative response budget needed per raw-block sync batch entry.
    const ZCASHD_COMPAT_SYNC_RESPONSE_BUDGET_BYTES_PER_BLOCK: u64 = 4 * 1024 * 1024;
    /// Extra time Zebra waits for the zcashd-compat supervisor task beyond the
    /// child's `shutdown_grace_period`. The supervisor's `terminate_child` waits
    /// the full grace period before its SIGKILL last resort, so the outer wait
    /// must be strictly longer or aborting the task races the graceful path.
    const ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN: std::time::Duration =
        std::time::Duration::from_secs(30);
    /// Default zcashd-compat RPC listen address when `--zcashd-compat` is enabled.
    fn zcashd_compat_default_rpc_listen_addr() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 28232))
    }

    fn zcashd_compat_cookie_path(config: &ZebradConfig) -> std::path::PathBuf {
        config
            .zcashd_compat
            .cookie_dir
            .join(&config.zcashd_compat.cookie_file_name)
    }

    fn zcashd_compat_rpc_config(config: &ZebradConfig) -> zebra_rpc::config::rpc::Config {
        let mut compat_rpc_config = config.rpc.clone();
        compat_rpc_config.listen_addr = config.zcashd_compat.listen_addr;
        compat_rpc_config.enable_cookie_auth = config.zcashd_compat.enable_cookie_auth;
        compat_rpc_config.cookie_dir = config.zcashd_compat.cookie_dir.clone();
        compat_rpc_config.cookie_file_name = config.zcashd_compat.cookie_file_name.clone();
        compat_rpc_config.tls = match (
            &config.zcashd_compat.tls_cert_file,
            &config.zcashd_compat.tls_key_file,
        ) {
            (Some(cert_file), Some(key_file)) => Some(zebra_rpc::config::rpc::TlsConfig {
                cert_file: cert_file.clone(),
                key_file: key_file.clone(),
            }),
            _ => None,
        };
        compat_rpc_config.max_response_body_size = compat_rpc_config
            .max_response_body_size
            .max(Self::ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE);
        compat_rpc_config
    }

    fn zcashd_compat_extra_arg_u64(
        config: &ZebradConfig,
        name: &str,
    ) -> Result<Option<u64>, Report> {
        let option_name = format!("-{name}=");
        let long_option_name = format!("--{name}=");
        let bare_option_name = format!("-{name}");
        let bare_long_option_name = format!("--{name}");

        config
            .zcashd_compat
            .zcashd_extra_args
            .iter()
            .filter_map(|arg| {
                arg.strip_prefix(&option_name)
                    .or_else(|| arg.strip_prefix(&long_option_name))
                    .map(|value| (arg, Some(value)))
                    .or_else(|| {
                        (arg == &bare_option_name || arg == &bare_long_option_name)
                            .then_some((arg, None))
                    })
            })
            .map(|(arg, value)| {
                let value = value.ok_or_else(|| {
                    eyre!("zcashd_compat.zcashd_extra_args contains {arg:?} without a value")
                })?;

                value.parse::<u64>().map_err(|error| {
                    eyre!(
                        "zcashd_compat.zcashd_extra_args contains invalid {name} value {value:?}: {error}"
                    )
                })
            })
            .next_back()
            .transpose()
    }

    fn validate_zcashd_compat_sync_batch_response_size(
        config: &ZebradConfig,
    ) -> Result<(), Report> {
        let sync_batch_size =
            Self::zcashd_compat_extra_arg_u64(config, "zebra-compat-sync-batch-size")?
                .unwrap_or(Self::ZCASHD_COMPAT_DEFAULT_SYNC_BATCH_SIZE);
        let sync_response_budget_mb =
            Self::zcashd_compat_extra_arg_u64(config, "zebra-compat-sync-response-budget-mb")?
                .unwrap_or(Self::ZCASHD_COMPAT_DEFAULT_SYNC_RESPONSE_BUDGET_MB)
                .max(1);
        let sync_response_budget_bytes = sync_response_budget_mb
            .checked_mul(Self::ZCASHD_COMPAT_MIB)
            .ok_or_else(|| {
                eyre!(
                    "zcashd-compat sync response budget {sync_response_budget_mb} MiB is too large"
                )
            })?;
        let max_batch_size_for_response_budget =
            if sync_response_budget_bytes <= Self::ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES {
                1
            } else {
                (sync_response_budget_bytes - Self::ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES)
                    / (2 * Self::ZCASHD_COMPAT_MAX_BLOCK_BYTES
                        + Self::ZCASHD_COMPAT_JSON_RPC_BLOCK_OVERHEAD_BYTES)
            }
            .max(1);
        let required_sync_response_budget_bytes =
            Self::ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES
                .checked_add(
                    sync_batch_size
                        .checked_mul(
                            2 * Self::ZCASHD_COMPAT_MAX_BLOCK_BYTES
                                + Self::ZCASHD_COMPAT_JSON_RPC_BLOCK_OVERHEAD_BYTES,
                        )
                        .ok_or_else(|| {
                            eyre!("zcashd-compat sync batch size {sync_batch_size} is too large")
                        })?,
                )
                .ok_or_else(|| {
                    eyre!("zcashd-compat sync batch size {sync_batch_size} is too large")
                })?;
        let required_sync_response_budget_mb =
            required_sync_response_budget_bytes.div_ceil(Self::ZCASHD_COMPAT_MIB);
        let required_max_response_body_size = sync_batch_size
            .checked_mul(Self::ZCASHD_COMPAT_SYNC_RESPONSE_BUDGET_BYTES_PER_BLOCK)
            .ok_or_else(|| eyre!("zcashd-compat sync batch size {sync_batch_size} is too large"))?;
        let required_max_response_body_size: usize = required_max_response_body_size
            .try_into()
            .map_err(|_| eyre!("zcashd-compat sync batch size {sync_batch_size} is too large"))?;
        let effective_max_response_body_size = config
            .rpc
            .max_response_body_size
            .max(Self::ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE);

        let mut errors = Vec::new();
        if sync_batch_size > max_batch_size_for_response_budget {
            errors.push(format!(
                "zcashd-compat sync batch size {sync_batch_size} requires \
                 zcashd_compat.zcashd_extra_args to include \
                 -zebra-compat-sync-response-budget-mb={required_sync_response_budget_mb} or higher; \
                 configured effective value is {sync_response_budget_mb}"
            ));
        }

        if effective_max_response_body_size < required_max_response_body_size {
            errors.push(format!(
                "zcashd-compat sync batch size {sync_batch_size} requires \
                 rpc.max_response_body_size = {required_max_response_body_size} or higher; \
                 configured effective value is {effective_max_response_body_size}"
            ));
        }

        if !errors.is_empty() {
            return Err(eyre!("{}", errors.join("\n")));
        }

        Ok(())
    }

    fn validate_zcashd_compat_tls_config(config: &ZebradConfig) -> Result<(), Report> {
        if config.zcashd_compat.tls_cert_file.is_some()
            != config.zcashd_compat.tls_key_file.is_some()
        {
            return Err(eyre!(
                "zcashd-compat TLS requires both zcashd_compat.tls_cert_file and zcashd_compat.tls_key_file"
            ));
        }

        if config.zcashd_compat.tls_ca_file.is_some() && !config.zcashd_compat.tls_enabled() {
            return Err(eyre!(
                "zcashd_compat.tls_ca_file requires zcashd-compat TLS with both zcashd_compat.tls_cert_file and zcashd_compat.tls_key_file"
            ));
        }

        for (name, path) in [
            (
                "zcashd_compat.tls_cert_file",
                &config.zcashd_compat.tls_cert_file,
            ),
            (
                "zcashd_compat.tls_key_file",
                &config.zcashd_compat.tls_key_file,
            ),
            (
                "zcashd_compat.tls_ca_file",
                &config.zcashd_compat.tls_ca_file,
            ),
        ] {
            if let Some(path) = path {
                std::fs::File::open(path)
                    .map_err(|error| eyre!("could not read {name}={}: {error}", path.display()))?;
            }
        }

        if let Some(listen_addr) = config.zcashd_compat.listen_addr {
            if !listen_addr.ip().is_loopback()
                && !config.zcashd_compat.tls_enabled()
                && !config.zcashd_compat.unsafe_allow_remote_http
            {
                return Err(eyre!(
                    "zcashd_compat.listen_addr={listen_addr} is non-loopback and requires TLS with both zcashd_compat.tls_cert_file and zcashd_compat.tls_key_file, \
                     or zcashd_compat.unsafe_allow_remote_http=true when a container or private network boundary secures the listener"
                ));
            }
        }

        if config.zcashd_compat.enabled
            && config.zcashd_compat.manage_zcashd
            && config.zcashd_compat.tls_enabled()
            && config.zcashd_compat.tls_ca_file.is_none()
        {
            return Err(eyre!(
                "zcashd-compat supervision with TLS requires zcashd_compat.tls_ca_file so zcashd can verify Zebra"
            ));
        }

        Ok(())
    }

    fn zcashd_compat_rpc_url(config: &ZebradConfig) -> Result<String, Report> {
        let listen_addr = config.zcashd_compat.listen_addr.ok_or_else(|| {
            eyre!("zcashd-compat mode requires zcashd_compat.listen_addr to be set")
        })?;
        let scheme = if config.zcashd_compat.tls_enabled() {
            "https"
        } else {
            "http"
        };
        Ok(format!("{scheme}://{listen_addr}"))
    }

    /// Returns the supervisor shutdown timeout when zcashd-compat `zcashd` supervision is active.
    ///
    /// This is the configured `shutdown_grace_period` plus a fixed margin, so the
    /// supervisor task always gets to finish its own SIGTERM → grace → SIGKILL
    /// sequence before Zebra gives up on the task.
    fn zcashd_compat_supervisor_shutdown_timeout(
        config: &ZebradConfig,
    ) -> Option<std::time::Duration> {
        (config.zcashd_compat.enabled && config.zcashd_compat.manage_zcashd).then_some(
            config
                .zcashd_compat
                .shutdown_grace_period
                .saturating_add(Self::ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN),
        )
    }

    async fn start(&self) -> Result<(), Report> {
        check_tcp_slow_start_after_idle();

        let config = APPLICATION.config();
        let is_regtest = config.network.network.is_regtest();

        let config = if is_regtest {
            Arc::new(ZebradConfig {
                mempool: mempool::Config {
                    debug_enable_at_height: Some(0),
                    ..config.mempool
                },
                ..Arc::unwrap_or_clone(config)
            })
        } else {
            config
        };

        if config.zcashd_compat.enabled {
            zcashd_compat::run_preflight(&config, self.unsafe_low_specs)?;
        }

        let resolved_zcashd_path = if config.zcashd_compat.enabled
            && config.zcashd_compat.manage_zcashd
        {
            let zcashd_compat_config = config.zcashd_compat.clone();
            let state_cache_dir = config.state.cache_dir.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    zcashd_compat::resolve_zcashd_binary_path(
                        &zcashd_compat_config,
                        &state_cache_dir,
                    )
                })
                .await
                .map_err(|err| eyre!("failed to join managed zcashd binary resolver: {err}"))??,
            )
        } else {
            None
        };

        info!("initializing node state");

        // Surface a misconfigured storage mode as a clean startup error, before the
        // (potentially slow) database open panics deep inside the state service.
        config
            .state
            .validate_storage_mode(&config.network.network)
            .map_err(|error| eyre!("invalid state storage configuration: {error}"))?;

        let (_, max_checkpoint_height) = zebra_consensus::router::init_checkpoint_list(
            config.consensus.clone(),
            &config.network.network,
        );

        info!("opening database, this may take a few minutes");

        let mut state_config = config.state.clone();
        state_config.enable_zakura_header_seed_from_committed_blocks = config.network.v2_p2p;

        let (state_service, read_only_state_service, latest_chain_tip, chain_tip_change) =
            zebra_state::init(
                state_config,
                &config.network.network,
                max_checkpoint_height,
                config.sync.checkpoint_verify_concurrency_limit
                    * (VERIFICATION_PIPELINE_SCALING_MULTIPLIER + 1),
            )
            .await;

        info!("logging database metrics on startup");
        read_only_state_service.log_db_metrics();

        let state = ServiceBuilder::new()
            .buffer(Self::state_buffer_bound())
            .service(state_service);

        let zakura_header_sync_driver_startup = if config.network.v2_p2p {
            Some(
                zakura_header_sync_driver_startup(
                    read_only_state_service.clone(),
                    &config.network.network,
                )
                .await?,
            )
        } else {
            None
        };

        info!("initializing network");
        // The service that our node uses to respond to requests by peers. The
        // load_shed middleware ensures that we reduce the size of the peer set
        // in response to excess load.
        //
        // # Security
        //
        // This layer stack is security-sensitive, modifying it can cause hangs,
        // or enable denial of service attacks.
        //
        // See `zebra_network::Connection::drive_peer_request()` for details.
        let (setup_tx, setup_rx) = oneshot::channel();
        let inbound = ServiceBuilder::new()
            .load_shed()
            .buffer(inbound::downloads::MAX_INBOUND_CONCURRENCY)
            .timeout(MAX_INBOUND_RESPONSE_TIME)
            .service(Inbound::new(
                config.sync.full_verify_concurrency_limit,
                setup_rx,
            ));

        // Pruned nodes can still make outbound requests, but they cannot serve
        // arbitrary historical blocks, so they must not advertise full-node service.
        let advertised_services = if config.state.pruning_config().is_some() {
            PeerServices::empty()
        } else {
            PeerServices::NODE_NETWORK
        };

        let (peer_set, address_book, misbehavior_sender, zakura_endpoint) =
            zebra_network::init_with_zakura_header_sync(
                config.network.clone(),
                inbound,
                latest_chain_tip.clone(),
                user_agent(),
                advertised_services,
                zakura_header_sync_driver_startup,
            )
            .await;

        // Start health server if configured (after sync_status is available)

        info!("initializing verifiers");
        let (tx_verifier_setup_tx, tx_verifier_setup_rx) = oneshot::channel();
        let (block_verifier_router, tx_verifier, consensus_task_handles, max_checkpoint_height) =
            zebra_consensus::router::init(
                config.consensus.clone(),
                &config.network.network,
                state.clone(),
                tx_verifier_setup_rx,
            )
            .await;

        if let Some(endpoint) = zakura_endpoint.clone() {
            if let (Some(header_sync), Some(shutdown), Some(actions)) = (
                endpoint.header_sync(),
                endpoint.header_sync_shutdown(),
                endpoint.take_header_sync_actions().await,
            ) {
                let driver_task = tokio::spawn(
                    drive_zakura_header_sync_actions(
                        actions,
                        ZakuraHeaderSyncDriverHandles {
                            endpoint: endpoint.clone(),
                            header_sync: header_sync.clone(),
                            block_sync: endpoint.block_sync(),
                        },
                        state.clone(),
                        read_only_state_service.clone(),
                        block_verifier_router.clone(),
                        shutdown.clone().cancelled_owned(),
                    )
                    .in_current_span(),
                );
                endpoint.push_header_sync_task(driver_task).await;

                if let (Some(block_sync), Some(block_actions)) = (
                    endpoint.block_sync(),
                    endpoint.take_block_sync_actions().await,
                ) {
                    let block_driver_task = tokio::spawn(
                        drive_block_sync_actions(
                            block_actions,
                            endpoint.supervisor(),
                            block_sync.clone(),
                            read_only_state_service.clone(),
                            block_verifier_router.clone(),
                            shutdown.clone().cancelled_owned(),
                        )
                        .in_current_span(),
                    );
                    endpoint.push_block_sync_task(block_driver_task).await;
                }

                let full_block_task = tokio::spawn(
                    mirror_zakura_full_block_commits(
                        chain_tip_change.clone(),
                        read_only_state_service.clone(),
                        header_sync,
                        endpoint.block_sync(),
                        shutdown.cancelled_owned(),
                    )
                    .in_current_span(),
                );
                endpoint.push_header_sync_task(full_block_task).await;
            }
        }

        info!("initializing syncer");
        let (mut syncer, sync_status) = ChainSync::new(
            &config,
            max_checkpoint_height,
            peer_set.clone(),
            block_verifier_router.clone(),
            state.clone(),
            latest_chain_tip.clone(),
            misbehavior_sender.clone(),
        );

        info!("initializing mempool");
        let (mempool, mempool_transaction_subscriber) = Mempool::new(
            &config.mempool,
            peer_set.clone(),
            state.clone(),
            tx_verifier,
            sync_status.clone(),
            latest_chain_tip.clone(),
            chain_tip_change.clone(),
            misbehavior_sender.clone(),
        );
        let mempool = BoxService::new(mempool);
        let mempool = ServiceBuilder::new()
            .buffer(mempool::downloads::MAX_INBOUND_CONCURRENCY)
            .service(mempool);

        if tx_verifier_setup_tx.send(mempool.clone()).is_err() {
            warn!("error setting up the transaction verifier with a handle to the mempool service");
        };

        info!("fully initializing inbound peer request handler");
        // Fully start the inbound service as soon as possible
        let setup_data = InboundSetupData {
            address_book: address_book.clone(),
            block_download_peer_set: peer_set.clone(),
            block_verifier: block_verifier_router.clone(),
            mempool: mempool.clone(),
            state: state.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            misbehavior_sender,
        };
        setup_tx
            .send(setup_data)
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;
        // And give it time to clear its queue
        tokio::task::yield_now().await;

        // Create a channel to send mined blocks to the gossip task
        let submit_block_channel = SubmitBlockChannel::new();

        // Launch RPC server
        let (rpc_impl, mut rpc_tx_queue_handle) = RpcImpl::new(
            config.network.network.clone(),
            config.mining.clone(),
            config.rpc.debug_force_finished_sync,
            build_version(),
            user_agent(),
            mempool.clone(),
            state.clone(),
            read_only_state_service.clone(),
            block_verifier_router.clone(),
            sync_status.clone(),
            latest_chain_tip.clone(),
            address_book.clone(),
            LAST_WARN_ERROR_LOG_SENDER.subscribe(),
            Some(submit_block_channel.sender()),
        );

        let rpc_task_handle = if config.rpc.listen_addr.is_some() {
            RpcServer::start(rpc_impl.clone(), config.rpc.clone())
                .await
                .expect("server should start")
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        let zcashd_compat_rpc_task_handle = if config.zcashd_compat.enabled {
            RpcServer::start(rpc_impl.clone(), Self::zcashd_compat_rpc_config(&config))
                .await
                .expect("zcashd-compat RPC server should start")
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        let zcashd_compat_shutdown_timeout =
            Self::zcashd_compat_supervisor_shutdown_timeout(&config);
        let (zcashd_compat_shutdown_tx, zcashd_compat_shutdown_rx) = watch::channel(false);
        let mut zcashd_compat_task_handle = if let Some(resolved_zcashd_path) = resolved_zcashd_path
        {
            let supervisor_config = zcashd_compat::SupervisorConfig::new(
                &config.zcashd_compat,
                resolved_zcashd_path,
                &config.state.cache_dir,
                config.network.network.kind(),
                Self::zcashd_compat_rpc_url(&config)?,
                Self::zcashd_compat_cookie_path(&config),
                Self::zcashd_compat_rpc_config(&config).max_response_body_size,
            );

            info!(
                rpc_url = %supervisor_config.rpc_url,
                cookie_file = %supervisor_config.cookie_path.display(),
                "zcashd-compat source enabled"
            );

            tokio::spawn(
                zcashd_compat::run_supervisor(supervisor_config, zcashd_compat_shutdown_rx)
                    .in_current_span(),
            )
        } else {
            if config.zcashd_compat.enabled {
                zcashd_compat::set_supervision_config_disabled_metrics();
                info!(
                    rpc_url = %Self::zcashd_compat_rpc_url(&config)?,
                    cookie_file = %Self::zcashd_compat_cookie_path(&config).display(),
                    "zcashd-compat source enabled: zcashd supervision disabled"
                );
            }

            tokio::spawn(std::future::pending().in_current_span())
        };

        // TODO: Add a shutdown signal and start the server with `serve_with_incoming_shutdown()` if
        //       any related unit tests sometimes crash with memory errors
        let indexer_rpc_task_handle = {
            if let Some(indexer_listen_addr) = config.rpc.indexer_listen_addr {
                info!("spawning indexer RPC server");
                let (indexer_rpc_task_handle, _listen_addr) = zebra_rpc::indexer::server::init(
                    indexer_listen_addr,
                    read_only_state_service.clone(),
                    latest_chain_tip.clone(),
                    mempool_transaction_subscriber.clone(),
                )
                .await
                .map_err(|err| eyre!(err))?;

                indexer_rpc_task_handle
            } else {
                warn!("configure an indexer_listen_addr to start the indexer RPC server");
                tokio::spawn(std::future::pending().in_current_span())
            }
        };

        // Start concurrent tasks which don't add load to other tasks
        info!("spawning block gossip task");
        let block_gossip_task_handle = tokio::spawn(
            sync::gossip_best_tip_block_hashes(
                sync_status.clone(),
                chain_tip_change.clone(),
                peer_set.clone(),
                Some(submit_block_channel.receiver()),
            )
            .in_current_span(),
        );

        info!("spawning mempool queue checker task");
        let mempool_queue_checker_task_handle = mempool::QueueChecker::spawn(mempool.clone());

        info!("spawning mempool transaction gossip task");
        let tx_gossip_task_handle = tokio::spawn(
            mempool::gossip_mempool_transaction_id(
                mempool_transaction_subscriber.subscribe(),
                peer_set.clone(),
            )
            .in_current_span(),
        );

        info!("spawning delete old databases task");
        let mut old_databases_task_handle = zebra_state::check_and_delete_old_state_databases(
            &config.state,
            &config.network.network,
        );

        info!("spawning progress logging task");
        let (chain_tip_metrics_sender, chain_tip_metrics_receiver) =
            health::ChainTipMetrics::channel();
        let progress_task_handle = tokio::spawn(
            show_block_chain_progress(
                config.network.network.clone(),
                latest_chain_tip.clone(),
                sync_status.clone(),
                chain_tip_metrics_sender,
            )
            .in_current_span(),
        );

        // Start health server if configured
        info!("initializing health endpoints");
        let (health_task_handle, _) = health::init(
            config.health.clone(),
            config.network.network.clone(),
            chain_tip_metrics_receiver,
            sync_status.clone(),
            address_book.clone(),
        )
        .await;

        // Spawn never ending end of support task.
        info!("spawning end of support checking task");
        let end_of_support_task_handle = tokio::spawn(
            sync::end_of_support::start(config.network.network.clone(), latest_chain_tip.clone())
                .in_current_span(),
        );

        // Give the inbound service more time to clear its queue,
        // then start concurrent tasks that can add load to the inbound service
        // (by opening more peer connections, so those peers send us requests)
        tokio::task::yield_now().await;

        // The crawler only activates immediately in tests that use mempool debug mode
        info!("spawning mempool crawler task");
        let mempool_crawler_task_handle = mempool::Crawler::spawn(
            &config.mempool,
            peer_set,
            mempool.clone(),
            sync_status.clone(),
            chain_tip_change.clone(),
        );

        info!("spawning syncer task");
        // In regtest, commit the genesis block directly (bypassing the syncer's genesis
        // download, which requires a connected peer). Then run the syncer normally so
        // that multi-hop block propagation works: gossiped blocks that arrive out of
        // order (e.g. only the latest tip hash was gossiped) will be recovered by the
        // syncer using block locators within REGTEST_SYNC_RESTART_DELAY (2 seconds).
        if is_regtest
            && !syncer
                .state_contains(config.network.network.genesis_hash())
                .await?
        {
            let genesis_hash = block_verifier_router
                .clone()
                .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
                .await
                .expect("should validate Regtest genesis block");

            assert_eq!(
                genesis_hash,
                config.network.network.genesis_hash(),
                "validated block hash should match network genesis hash"
            )
        }
        let replace_legacy_syncer = replace_legacy_syncer_on_zakura_path(&config.network);
        let syncer_task_handle = if replace_legacy_syncer {
            info!("Zakura block sync is replacing the legacy ChainSync body downloader");
            tokio::spawn(std::future::pending::<Result<(), Report>>().in_current_span())
        } else {
            tokio::spawn(syncer.sync().in_current_span())
        };

        // And finally, spawn the internal Zcash miner, if it is enabled.
        //
        // TODO: add a config to enable the miner rather than a feature.
        #[cfg(feature = "internal-miner")]
        let miner_task_handle = if config.mining.is_internal_miner_enabled() {
            info!("spawning Zcash miner");
            components::miner::spawn_init(&config.metrics, rpc_impl)
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        #[cfg(not(feature = "internal-miner"))]
        // Spawn a dummy miner task which doesn't do anything and never finishes.
        let miner_task_handle: tokio::task::JoinHandle<Result<(), Report>> =
            tokio::spawn(std::future::pending().in_current_span());

        info!("spawned initial Zebra tasks");

        // TODO: put tasks into an ongoing FuturesUnordered and a startup FuturesUnordered?

        // ongoing tasks
        pin!(rpc_task_handle);
        pin!(zcashd_compat_rpc_task_handle);
        pin!(indexer_rpc_task_handle);
        pin!(syncer_task_handle);
        pin!(block_gossip_task_handle);
        pin!(mempool_crawler_task_handle);
        pin!(mempool_queue_checker_task_handle);
        pin!(tx_gossip_task_handle);
        pin!(progress_task_handle);
        pin!(end_of_support_task_handle);
        pin!(miner_task_handle);

        // startup tasks
        let BackgroundTaskHandles {
            mut state_checkpoint_verify_handle,
        } = consensus_task_handles;

        let state_checkpoint_verify_handle_fused = (&mut state_checkpoint_verify_handle).fuse();
        pin!(state_checkpoint_verify_handle_fused);

        let old_databases_task_handle_fused = (&mut old_databases_task_handle).fuse();
        pin!(old_databases_task_handle_fused);

        // Wait for tasks to finish
        let mut zcashd_compat_task_finished = false;
        let exit_status = {
            let zcashd_compat_task_handle_fused = (&mut zcashd_compat_task_handle).fuse();
            pin!(zcashd_compat_task_handle_fused);

            loop {
                let mut exit_when_task_finishes = true;

                let result = select! {
                rpc_join_result = &mut rpc_task_handle => {
                    let rpc_server_result = rpc_join_result
                        .expect("unexpected panic in the rpc task");
                    info!(?rpc_server_result, "rpc task exited");
                    Ok(())
                }

                zcashd_compat_rpc_join_result = &mut zcashd_compat_rpc_task_handle => {
                    let compat_rpc_server_result = zcashd_compat_rpc_join_result
                        .expect("unexpected panic in the zcashd-compat rpc task");
                    info!(?compat_rpc_server_result, "zcashd-compat rpc task exited");
                    Ok(())
                }

                rpc_tx_queue_result = &mut rpc_tx_queue_handle => {
                    rpc_tx_queue_result
                        .expect("unexpected panic in the rpc transaction queue task");
                    info!("rpc transaction queue task exited");
                    Ok(())
                }

                indexer_rpc_join_result = &mut indexer_rpc_task_handle => {
                    let indexer_rpc_server_result = indexer_rpc_join_result
                        .expect("unexpected panic in the indexer task");
                    info!(?indexer_rpc_server_result, "indexer rpc task exited");
                    Ok(())
                }

                sync_result = &mut syncer_task_handle => sync_result
                    .expect("unexpected panic in the syncer task")
                    .map(|_| info!("syncer task exited")),

                block_gossip_result = &mut block_gossip_task_handle => block_gossip_result
                    .expect("unexpected panic in the chain tip block gossip task")
                    .map(|_| info!("chain tip block gossip task exited"))
                    .map_err(|e| eyre!(e)),

                mempool_crawl_result = &mut mempool_crawler_task_handle => mempool_crawl_result
                    .expect("unexpected panic in the mempool crawler")
                    .map(|_| info!("mempool crawler task exited"))
                    .map_err(|e| eyre!(e)),

                mempool_queue_result = &mut mempool_queue_checker_task_handle => mempool_queue_result
                    .expect("unexpected panic in the mempool queue checker")
                    .map(|_| info!("mempool queue checker task exited"))
                    .map_err(|e| eyre!(e)),

                tx_gossip_result = &mut tx_gossip_task_handle => tx_gossip_result
                    .expect("unexpected panic in the transaction gossip task")
                    .map(|_| info!("transaction gossip task exited"))
                    .map_err(|e| eyre!(e)),

                // The progress task runs forever, unless it panics.
                // So we don't need to provide an exit status for it.
                progress_result = &mut progress_task_handle => {
                    info!("chain progress task exited");
                    progress_result
                        .expect("unexpected panic in the chain progress task");
                }

                end_of_support_result = &mut end_of_support_task_handle => end_of_support_result
                    .expect("unexpected panic in the end of support task")
                    .map(|_| info!("end of support task exited")),

                // We also expect the state checkpoint verify task to finish.
                state_checkpoint_verify_result = &mut state_checkpoint_verify_handle_fused => {
                    state_checkpoint_verify_result
                        .unwrap_or_else(|_| panic!(
                            "unexpected panic checking previous state followed the best chain"));

                    exit_when_task_finishes = false;
                    Ok(())
                }

                // And the old databases task should finish while Zebra is running.
                old_databases_result = &mut old_databases_task_handle_fused => {
                    old_databases_result
                        .unwrap_or_else(|_| panic!(
                            "unexpected panic deleting old database directories"));

                    exit_when_task_finishes = false;
                    Ok(())
                }

                miner_result = &mut miner_task_handle => miner_result
                    .expect("unexpected panic in the miner task")
                    .map(|_| info!("miner task exited")),

                    zcashd_compat_result = &mut zcashd_compat_task_handle_fused => {
                        zcashd_compat_task_finished = true;
                        exit_when_task_finishes =
                            Self::zcashd_compat_supervisor_should_exit(zcashd_compat_result);
                        Ok(())
                    },
                };

                // Stop Zebra if a task finished and returned an error,
                // or if an ongoing task exited.
                if let Err(err) = result {
                    break Err(err);
                }

                if exit_when_task_finishes {
                    break Ok(());
                }
            }
        };

        info!("exiting Zebra because an ongoing task exited: asking other tasks to stop");

        // ongoing tasks
        rpc_task_handle.abort();
        zcashd_compat_rpc_task_handle.abort();
        rpc_tx_queue_handle.abort();
        health_task_handle.abort();
        syncer_task_handle.abort();
        block_gossip_task_handle.abort();
        mempool_crawler_task_handle.abort();
        mempool_queue_checker_task_handle.abort();
        tx_gossip_task_handle.abort();
        progress_task_handle.abort();
        end_of_support_task_handle.abort();
        miner_task_handle.abort();
        if zcashd_compat_task_finished {
            debug!("zcashd-compat supervisor task already exited before shutdown");
        } else if let Some(zcashd_compat_shutdown_timeout) = zcashd_compat_shutdown_timeout {
            info!(
                ?zcashd_compat_shutdown_timeout,
                "requesting zcashd-compat supervisor shutdown"
            );
            if zcashd_compat_shutdown_tx.send(true).is_err() {
                warn!("zcashd-compat supervisor shutdown request was not delivered");
            }
            if tokio::time::timeout(
                zcashd_compat_shutdown_timeout,
                &mut zcashd_compat_task_handle,
            )
            .await
            .is_err()
            {
                warn!(
                    ?zcashd_compat_shutdown_timeout,
                    "zcashd-compat supervisor did not finish before shutdown timeout; \
                     abandoning child process handle"
                );
                // The supervisor spawns zcashd without kill_on_drop, so this
                // abort abandons an already-signalled child rather than
                // SIGKILLing it mid-flush.
                zcashd_compat_task_handle.abort();
            }
        } else {
            debug!("aborting zcashd-compat supervisor task without managed child shutdown");
            zcashd_compat_task_handle.abort();
        }

        // startup tasks
        state_checkpoint_verify_handle.abort();
        old_databases_task_handle.abort();

        info!(
            "exiting Zebra: all tasks have been asked to stop, waiting for remaining tasks to finish"
        );

        exit_status
    }

    /// Returns `false` so Zebra keeps running if zcashd-compat supervision exits unexpectedly.
    fn zcashd_compat_supervisor_should_exit(
        zcashd_compat_result: Result<Result<(), Report>, tokio::task::JoinError>,
    ) -> bool {
        zcashd_compat::set_supervision_unexpectedly_disabled_metrics();

        match zcashd_compat_result {
            Ok(Ok(())) => {
                warn!(
                    "zcashd-compat supervisor task exited unexpectedly in supervision mode; continuing without zcashd supervision"
                );
            }
            Ok(Err(err)) => {
                warn!(
                    ?err,
                    "zcashd-compat supervisor task failed in supervision mode; continuing without zcashd supervision"
                );
            }
            Err(join_err) => {
                warn!(
                    ?join_err,
                    "zcashd-compat supervisor task panicked in supervision mode; continuing without zcashd supervision"
                );
            }
        }

        false
    }

    /// Returns the bound for the state service buffer,
    /// based on the configurations of the services that use the state concurrently.
    fn state_buffer_bound() -> usize {
        let config = APPLICATION.config();

        // Ignore the checkpoint verify limit, because it is very large.
        //
        // TODO: do we also need to account for concurrent use across services?
        //       we could multiply the maximum by 3/2, or add a fixed constant
        [
            config.sync.download_concurrency_limit,
            config.sync.full_verify_concurrency_limit,
            inbound::downloads::MAX_INBOUND_CONCURRENCY,
            mempool::downloads::MAX_INBOUND_CONCURRENCY,
        ]
        .into_iter()
        .max()
        .unwrap()
    }
}

impl Runnable for StartCmd {
    /// Start the application.
    fn run(&self) {
        info!("Starting zebrad");
        let rt = APPLICATION
            .state()
            .components_mut()
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .run(self.start());

        info!("stopping zebrad");
    }
}

impl config::Override<ZebradConfig> for StartCmd {
    // Process the given command line options, overriding settings from
    // a configuration file using explicit flags taken from command-line
    // arguments.
    fn override_config(&self, mut config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        if !self.filters.is_empty() {
            config.tracing.filter = Some(self.filters.join(","));
        }

        // `--zcashd-compat` is a one-way override that enables zcashd-compat mode.
        // The actual zcashd-compat guardrails are applied below using
        // `config.zcashd_compat.enabled` so CLI and config-file activation share one path.
        if self.zcashd_compat {
            config.zcashd_compat.enabled = true;
        }

        if config.zcashd_compat.enabled {
            if config.zcashd_compat.listen_addr.is_none() {
                config.zcashd_compat.listen_addr =
                    Some(Self::zcashd_compat_default_rpc_listen_addr());
            }

            if let (Some(rpc_listen_addr), Some(compat_listen_addr)) =
                (config.rpc.listen_addr, config.zcashd_compat.listen_addr)
            {
                if rpc_listen_addr == compat_listen_addr {
                    return Err(std::io::Error::other(format!(
                        "zcashd-compat mode requires different RPC listen addresses: \
                         rpc.listen_addr={rpc_listen_addr} conflicts with \
                         zcashd_compat.listen_addr={compat_listen_addr}"
                    ))
                    .into());
                }
            }

            Self::validate_zcashd_compat_tls_config(&config)
                .map_err(|err| std::io::Error::other(err.to_string()))?;

            if !config.zcashd_compat.enable_cookie_auth && !config.zcashd_compat.tls_enabled() {
                return Err(std::io::Error::other(
                    "zcashd_compat.enable_cookie_auth=false requires TLS on the zcashd-compat RPC listener",
                )
                .into());
            }

            Self::validate_zcashd_compat_sync_batch_response_size(&config)
                .map_err(|err| std::io::Error::other(err.to_string()))?;

            if config.zcashd_compat.manage_zcashd {
                match zcashd_compat::effective_zcashd_source(&config.zcashd_compat) {
                    Ok(zcashd_compat::ZcashdBinarySource::Path(path))
                        if !zcashd_compat::is_command_resolvable(Path::new(&path)) =>
                    {
                        return Err(std::io::Error::other(format!(
                            "zcashd-compat mode could not resolve zcashd_path={}",
                            path.display()
                        ))
                        .into());
                    }
                    Ok(_) => {}
                    Err(err) => return Err(std::io::Error::other(err.to_string()).into()),
                }
            }
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use abscissa_core::config::Override;
    use color_eyre::eyre::eyre;

    use super::StartCmd;
    use crate::components::zcashd_compat;
    use crate::config::ZebradConfig;

    #[test]
    fn zcashd_compat_flag_applies_rpc_guardrails() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = false;
        config.rpc.listen_addr = None;

        let config = cmd
            .override_config(config)
            .expect("zcashd-compat override config should succeed");

        assert!(config.zcashd_compat.enabled);
        assert_eq!(
            config.zcashd_compat.listen_addr,
            Some(StartCmd::zcashd_compat_default_rpc_listen_addr())
        );
        assert_eq!(config.rpc.listen_addr, None);
    }

    #[test]
    fn zcashd_compat_config_applies_rpc_guardrails() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.rpc.listen_addr = None;

        let config = cmd
            .override_config(config)
            .expect("zcashd-compat override config should succeed");

        assert!(config.zcashd_compat.enabled);
        assert_eq!(
            config.zcashd_compat.listen_addr,
            Some(StartCmd::zcashd_compat_default_rpc_listen_addr())
        );
        assert_eq!(config.rpc.listen_addr, None);
    }

    #[test]
    fn zcashd_compat_flag_rejects_conflicting_rpc_listen_addr() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.rpc.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.manage_zcashd = false;

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat should reject overlapping RPC listen addresses");

        assert!(
            error
                .to_string()
                .contains("requires different RPC listen addresses"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_conflicting_rpc_listen_addr() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.rpc.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat should reject overlapping configured RPC listen addresses");

        assert!(
            error
                .to_string()
                .contains("requires different RPC listen addresses"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_rpc_config_uses_dedicated_cookie_and_min_response_size() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.cookie_dir = "/tmp/zcashd-compat-cookie-dir".into();
        config.zcashd_compat.cookie_file_name = ".zcashd-compat.cookie".to_string();
        config.rpc.cookie_dir = "/tmp/standard-rpc-cookie-dir".into();
        config.rpc.cookie_file_name = ".cookie".to_string();
        config.rpc.max_response_body_size = 1024;

        let compat_rpc_config = StartCmd::zcashd_compat_rpc_config(&config);
        assert_eq!(
            compat_rpc_config.listen_addr,
            config.zcashd_compat.listen_addr
        );
        assert_eq!(
            compat_rpc_config.cookie_dir,
            config.zcashd_compat.cookie_dir
        );
        assert_eq!(
            compat_rpc_config.cookie_file_name,
            config.zcashd_compat.cookie_file_name
        );
        assert!(compat_rpc_config.enable_cookie_auth);
        assert_eq!(
            compat_rpc_config.max_response_body_size,
            StartCmd::ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE
        );
    }

    #[test]
    fn zcashd_compat_rpc_config_can_disable_cookie_auth() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.enable_cookie_auth = false;

        let compat_rpc_config = StartCmd::zcashd_compat_rpc_config(&config);

        assert!(!compat_rpc_config.enable_cookie_auth);
    }

    #[test]
    fn zcashd_compat_rpc_url_uses_https_when_tls_enabled() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.tls_cert_file = Some("/tmp/zebra.crt".into());
        config.zcashd_compat.tls_key_file = Some("/tmp/zebra.key".into());

        let rpc_url =
            StartCmd::zcashd_compat_rpc_url(&config).expect("zcashd-compat RPC URL should format");

        assert!(rpc_url.starts_with("https://"));
    }

    #[test]
    fn zcashd_compat_config_rejects_non_loopback_listen_addr_without_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([192, 0, 2, 1], 28232).into());

        let error = cmd
            .override_config(config)
            .expect_err("non-loopback zcashd-compat RPC should require TLS");

        assert!(
            error
                .to_string()
                .contains("listen_addr=192.0.2.1:28232 is non-loopback and requires TLS"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_loopback_listen_addr_without_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([127, 0, 0, 1], 28232).into());

        cmd.override_config(config)
            .expect("loopback zcashd-compat RPC should allow plain HTTP");
    }

    #[test]
    fn zcashd_compat_config_allows_non_loopback_listen_addr_with_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([192, 0, 2, 1], 28232).into());
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);

        cmd.override_config(config)
            .expect("non-loopback zcashd-compat RPC should be allowed with TLS");
    }

    #[test]
    fn zcashd_compat_config_allows_non_loopback_listen_addr_with_unsafe_override() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([192, 0, 2, 1], 28232).into());
        config.zcashd_compat.unsafe_allow_remote_http = true;

        cmd.override_config(config).expect(
            "non-loopback zcashd-compat RPC should be allowed with the explicit unsafe override",
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_no_cookie_auth_without_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.enable_cookie_auth = false;

        let error = cmd
            .override_config(config)
            .expect_err("no-cookie zcashd-compat RPC should require TLS");

        assert!(
            error
                .to_string()
                .contains("enable_cookie_auth=false requires TLS"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_no_cookie_auth_with_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.enable_cookie_auth = false;
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);

        cmd.override_config(config)
            .expect("no-cookie zcashd-compat RPC should be allowed with TLS");
    }

    #[test]
    fn zcashd_compat_config_rejects_missing_tls_files() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.tls_cert_file = Some("/tmp/zebra-missing.crt".into());
        config.zcashd_compat.tls_key_file = Some("/tmp/zebra-missing.key".into());

        let error = cmd
            .override_config(config)
            .expect_err("missing TLS files should be rejected");
        assert!(
            error.to_string().contains("could not read"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_tls_ca_without_tls_listener() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let ca_file = tempdir.path().join("ca.pem");
        std::fs::write(&ca_file, "placeholder ca").expect("CA file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.tls_ca_file = Some(ca_file);

        let error = cmd
            .override_config(config)
            .expect_err("CA file should require an enabled TLS listener");
        assert!(
            error.to_string().contains("tls_ca_file requires"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_managed_tls_without_ca_file() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Managed;
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);

        let error = cmd
            .override_config(config)
            .expect_err("managed TLS should require a CA file");
        assert!(
            error.to_string().contains("tls_ca_file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_managed_tls_with_ca_file() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        let ca_file = tempdir.path().join("ca.pem");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        std::fs::write(&ca_file, "placeholder ca").expect("CA file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Managed;
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);
        config.zcashd_compat.tls_ca_file = Some(ca_file);

        cmd.override_config(config)
            .expect("managed zcashd-compat RPC should accept TLS with a CA file");
    }

    #[test]
    fn zcashd_compat_config_rejects_incomplete_tls_pair() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.tls_cert_file = Some("/tmp/zebra.crt".into());

        let error = cmd
            .override_config(config)
            .expect_err("TLS should require both cert and key files");

        assert!(
            error.to_string().contains("tls_cert_file")
                && error.to_string().contains("tls_key_file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_large_sync_batch_without_matching_rpc_response_size() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args =
            vec!["-zebra-compat-sync-batch-size=80".to_string()];

        let error = cmd
            .override_config(config)
            .expect_err("large zcashd sync batches should require a matching RPC response limit");

        assert!(
            error
                .to_string()
                .contains("rpc.max_response_body_size = 335544320"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_large_sync_batch_with_matching_rpc_response_size() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args = vec![
            "-zebra-compat-sync-batch-size=80".to_string(),
            "-zebra-compat-sync-response-budget-mb=320".to_string(),
        ];
        config.rpc.max_response_body_size = 320 * 1024 * 1024;

        cmd.override_config(config)
            .expect("large zcashd sync batches should allow a matching RPC response limit");
    }

    #[test]
    fn zcashd_compat_config_rejects_large_sync_batch_without_matching_zcashd_response_budget() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args =
            vec!["-zebra-compat-sync-batch-size=80".to_string()];
        config.rpc.max_response_body_size = 320 * 1024 * 1024;

        let error = cmd.override_config(config).expect_err(
            "large zcashd sync batches should require a matching zcashd response budget",
        );

        assert!(
            error
                .to_string()
                .contains("-zebra-compat-sync-response-budget-mb=307"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_invalid_sync_batch_size() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args =
            vec!["-zebra-compat-sync-batch-size=eighty".to_string()];

        let error = cmd
            .override_config(config)
            .expect_err("invalid zcashd sync batch sizes should be rejected");

        assert!(
            error
                .to_string()
                .contains("invalid zebra-compat-sync-batch-size value"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_cookie_path_uses_compat_cookie_dir() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.cookie_dir = "/tmp/zcashd-compat-cookie-dir".into();
        config.zcashd_compat.cookie_file_name = ".zcashd-compat.cookie".to_string();
        config.rpc.cookie_dir = "/tmp/standard-rpc-cookie-dir".into();
        config.rpc.cookie_file_name = ".cookie".to_string();

        assert_eq!(
            StartCmd::zcashd_compat_cookie_path(&config),
            std::path::PathBuf::from("/tmp/zcashd-compat-cookie-dir/.zcashd-compat.cookie")
        );
    }

    #[test]
    fn zcashd_compat_manage_zcashd_requires_resolvable_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_path = Some("/definitely/missing/zcashd-compat".into());

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat override should fail for an unresolvable zcashd path");

        assert!(
            error
                .to_string()
                .contains("zcashd-compat mode could not resolve zcashd_path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_path_source_requires_explicit_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Path;
        config.zcashd_compat.zcashd_path = None;

        let error = cmd
            .override_config(config)
            .expect_err("path source should require explicit zcashd_path");
        assert!(
            error.to_string().contains("zcashd_source=path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_managed_source_allows_missing_local_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Managed;
        config.zcashd_compat.zcashd_path = None;

        cmd.override_config(config)
            .expect("managed source should be validated at runtime, not override-time");
    }

    #[test]
    fn zcashd_compat_config_manage_zcashd_requires_resolvable_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_path = Some("/definitely/missing/zcashd-compat".into());

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat config should fail for an unresolvable zcashd path");

        assert!(
            error
                .to_string()
                .contains("zcashd-compat mode could not resolve zcashd_path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_supervisor_shutdown_timeout_matches_config() {
        let mut config = ZebradConfig::default();

        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.shutdown_grace_period = std::time::Duration::from_secs(42);
        assert_eq!(
            StartCmd::zcashd_compat_supervisor_shutdown_timeout(&config),
            Some(
                std::time::Duration::from_secs(42)
                    + StartCmd::ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN
            ),
            "outer supervisor wait must exceed the child grace period so task \
             abort cannot preempt graceful termination",
        );

        config.zcashd_compat.manage_zcashd = false;
        assert_eq!(
            StartCmd::zcashd_compat_supervisor_shutdown_timeout(&config),
            None
        );

        config.zcashd_compat.enabled = false;
        config.zcashd_compat.manage_zcashd = true;
        assert_eq!(
            StartCmd::zcashd_compat_supervisor_shutdown_timeout(&config),
            None
        );
    }

    #[test]
    fn zcashd_compat_supervisor_ok_exit_does_not_exit_zebra() {
        assert!(!StartCmd::zcashd_compat_supervisor_should_exit(Ok(Ok(()))));
    }

    #[test]
    fn zcashd_compat_supervisor_error_does_not_exit_zebra() {
        assert!(!StartCmd::zcashd_compat_supervisor_should_exit(Ok(Err(
            eyre!("simulated zcashd supervisor runtime failure"),
        ))));
    }

    #[tokio::test]
    async fn zcashd_compat_supervisor_panic_does_not_exit_zebra() {
        let join_err = tokio::spawn(async {
            panic!("simulated zcashd supervisor panic");
        })
        .await
        .expect_err("task should panic");

        assert!(!StartCmd::zcashd_compat_supervisor_should_exit(Err(
            join_err
        )));
    }
}

#[cfg(test)]
mod zakura_header_sync_driver_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    use tower::service_fn;
    use zebra_chain::serialization::ZcashDeserializeInto;
    use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES};

    fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
        Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
    }

    fn block_sync_startup_for_test() -> zebra_network::zakura::BlockSyncStartup {
        let (tip_tx, tip_rx) =
            tokio::sync::watch::channel((block::Height(0), block::Hash([0; 32])));
        drop(tip_tx);
        zebra_network::zakura::BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            },
            (block::Height(0), block::Hash([0; 32])),
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        )
    }

    #[test]
    fn replace_legacy_syncer_gate_is_zakura_only_and_default_safe() {
        let mut config = zebra_network::Config::default();

        assert!(!replace_legacy_syncer_on_zakura_path(&config));

        config.zakura.block_sync.replace_legacy_syncer = true;
        assert!(replace_legacy_syncer_on_zakura_path(&config));

        config.v2_p2p = false;
        assert!(!replace_legacy_syncer_on_zakura_path(&config));
    }

    #[test]
    fn served_header_body_size_hints_align_with_served_heights() {
        let start = block::Height(10);
        let header_heights = [
            block::Height(10),
            block::Height(11),
            block::Height(12),
            block::Height(13),
        ];
        let body_size_hints = [
            (block::Height(10), Some(100)),
            (block::Height(11), None),
            (block::Height(12), Some(300)),
            (block::Height(13), Some(400)),
        ];

        assert_eq!(
            body_sizes_for_served_header_range(start, header_heights, &body_size_hints),
            vec![100, 0, 300, 400],
        );

        assert_eq!(
            body_sizes_for_served_header_range(start, header_heights, &[]),
            vec![0, 0, 0, 0],
        );
    }

    #[test]
    fn block_verify_error_duplicate_classifier_detects_router_and_block_errors() {
        let hash = block::Hash([1; 32]);
        let duplicate_block_error = zebra_consensus::VerifyBlockError::Block {
            source: zebra_consensus::BlockError::AlreadyInChain(
                hash,
                zebra_state::KnownBlock::BestChain,
            ),
        };
        assert!(block_verify_error_is_duplicate(&duplicate_block_error));

        let duplicate_router_error = zebra_consensus::RouterError::Block {
            source: Box::new(zebra_consensus::VerifyBlockError::Block {
                source: zebra_consensus::BlockError::AlreadyInChain(
                    hash,
                    zebra_state::KnownBlock::BestChain,
                ),
            }),
        };
        assert!(block_verify_error_is_duplicate(&duplicate_router_error));

        let invalid_block_error = zebra_consensus::VerifyBlockError::Block {
            source: zebra_consensus::BlockError::NoTransactions,
        };
        assert!(!block_verify_error_is_duplicate(&invalid_block_error));
    }

    #[test]
    fn block_sync_missing_body_window_stays_inside_reorg_bound() {
        assert_eq!(
            block_sync_missing_body_window(block::Height(10), block::Height(10)),
            None
        );
        assert_eq!(
            block_sync_missing_body_window(block::Height(10), block::Height(12)),
            Some((block::Height(11), 2))
        );
        assert_eq!(
            block_sync_missing_body_window(
                block::Height(10),
                block::Height(10 + zebra_state::MAX_BLOCK_REORG_HEIGHT + 100)
            ),
            Some((block::Height(11), zebra_state::MAX_BLOCK_REORG_HEIGHT))
        );
    }

    #[test]
    fn block_sync_needed_blocks_align_missing_hashes_and_size_hints() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let headers = vec![
            (block::Height(1), block1.hash(), block1.header.clone()),
            (block::Height(2), block2.hash(), block2.header.clone()),
        ];
        let hints = vec![(block::Height(1), Some(0)), (block::Height(2), Some(42))];

        let needed = block_sync_needed_blocks_from_state(
            vec![block::Height(1), block::Height(2), block::Height(3)],
            headers,
            hints,
        );

        assert_eq!(
            needed,
            vec![
                BlockSyncBlockMeta {
                    height: block::Height(1),
                    hash: block1.hash(),
                    size: BlockSizeEstimate::Unknown,
                },
                BlockSyncBlockMeta {
                    height: block::Height(2),
                    hash: block2.hash(),
                    size: BlockSizeEstimate::Advertised(42),
                },
            ]
        );
    }

    #[test]
    fn block_sync_misbehavior_classifier_keeps_soft_reasons_soft() {
        assert!(!block_sync_misbehavior_is_hard(
            BlockSyncMisbehavior::SizeMismatch
        ));
        assert!(!block_sync_misbehavior_is_hard(
            BlockSyncMisbehavior::RangeUnavailable
        ));
        assert!(!block_sync_misbehavior_is_hard(
            BlockSyncMisbehavior::GetBlocksSpam
        ));
        assert!(block_sync_misbehavior_is_hard(
            BlockSyncMisbehavior::InvalidBlock
        ));
    }

    #[test]
    fn block_sync_chain_tip_action_mapping_preserves_reset_vs_grow() {
        let frontiers = BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: block::Hash([1; 32]),
        };
        let tip_block = zebra_state::ChainTipBlock {
            hash: block::Hash([1; 32]),
            height: block::Height(1),
            time: chrono::Utc::now(),
            transactions: Vec::new(),
            transaction_hashes: Arc::<[zebra_chain::transaction::Hash]>::from([]),
            previous_block_hash: block::Hash([0; 32]),
        };

        assert!(matches!(
            block_sync_chain_tip_event(
                &zebra_state::TipAction::Grow {
                    block: tip_block.clone()
                },
                frontiers
            ),
            BlockSyncEvent::ChainTipGrow(mapped) if mapped == frontiers
        ));
        assert!(matches!(
            block_sync_chain_tip_event(
                &zebra_state::TipAction::Reset {
                    height: block::Height(1),
                    hash: block::Hash([1; 32]),
                },
                frontiers
            ),
            BlockSyncEvent::ChainTipReset(mapped) if mapped == frontiers
        ));
    }

    #[tokio::test]
    async fn header_tip_notification_drives_block_sync_needed_query() {
        let startup = block_sync_startup_for_test();
        let (block_sync, mut reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let header_hash = block::Hash([2; 32]);

        notify_block_sync_header_tip(Some(&block_sync), block::Height(2), header_hash).await;

        let action = tokio::time::timeout(Duration::from_secs(5), reactor_actions.recv())
            .await
            .expect("reactor emits a needed-block query after a header tip")
            .expect("reactor action channel remains open");

        assert!(matches!(
            action,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(2),
            }
        ));

        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_answers_needed_block_queries_from_state() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);

        let expected_headers = Arc::new(vec![
            (block::Height(1), block1.hash(), block1.header.clone()),
            (block::Height(2), block2.hash(), block2.header.clone()),
        ]);
        let expected_hints = Arc::new(vec![
            (block::Height(1), Some(0)),
            (block::Height(2), Some(42)),
        ]);
        let read_requests = Arc::new(Mutex::new(Vec::new()));
        let read_requests_for_service = read_requests.clone();
        let read_headers = expected_headers.clone();
        let read_hints = expected_hints.clone();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let read_requests = read_requests_for_service.clone();
            let read_headers = read_headers.clone();
            let read_hints = read_hints.clone();
            async move {
                read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .push(request.clone());
                match request {
                    zebra_state::ReadRequest::MissingBlockBodies { from, limit } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(limit, 2);
                        Ok(zebra_state::ReadResponse::MissingBlockBodies(vec![
                            block::Height(1),
                            block::Height(2),
                        ]))
                    }
                    zebra_state::ReadRequest::HeadersByHeightRange { start, count } => {
                        assert_eq!(start, block::Height(1));
                        assert_eq!(count, 2);
                        Ok(zebra_state::ReadResponse::Headers((*read_headers).clone()))
                    }
                    zebra_state::ReadRequest::BlockSizeHints { from, count } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(count, 2);
                        Ok(zebra_state::ReadResponse::BlockSizeHints(
                            (*read_hints).clone(),
                        ))
                    }
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });

        let (commit_tx, mut commit_rx) = mpsc::channel(1);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let commit_tx = commit_tx.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        let hash = block.hash();
                        commit_tx
                            .send(hash)
                            .await
                            .expect("test commit receiver stays open");
                        Ok::<_, zebra_consensus::BoxError>(hash)
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            block_sync,
            read_state,
            verifier,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(2),
            })
            .await
            .expect("driver action channel stays open");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .len()
                    >= 3
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("driver answers QueryNeededBlocks through state reads");

        assert_eq!(
            read_requests
                .lock()
                .expect("test read request log is not poisoned")
                .as_slice(),
            &[
                zebra_state::ReadRequest::MissingBlockBodies {
                    from: block::Height(1),
                    limit: 2,
                },
                zebra_state::ReadRequest::HeadersByHeightRange {
                    start: block::Height(1),
                    count: 2,
                },
                zebra_state::ReadRequest::BlockSizeHints {
                    from: block::Height(1),
                    count: 2,
                },
            ]
        );

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("commit arrives after query"),
            Some(block1.hash())
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_commits_parent_first_and_ignores_outbound_actions() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let commit_tx = commit_tx.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        let hash = block.hash();
                        commit_tx
                            .send(hash)
                            .await
                            .expect("test commit receiver stays open");
                        Ok::<_, zebra_consensus::BoxError>(hash)
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let read_state = service_fn(|request: zebra_state::ReadRequest| async move {
            panic!("read_state should not be called for successful commit: {request:?}");
            #[allow(unreachable_code)]
            Ok::<zebra_state::ReadResponse, zebra_state::BoxError>(zebra_state::ReadResponse::Tip(
                None,
            ))
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            block_sync,
            read_state,
            verifier,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let peer =
            zebra_network::zakura::ZakuraPeerId::new(vec![8; 32]).expect("test peer id is valid");
        action_tx
            .send(BlockSyncAction::SendMessage {
                peer,
                msg: zebra_network::zakura::BlockSyncMessage::Status(
                    zebra_network::zakura::BlockSyncStatus::default(),
                ),
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                block: block2.clone(),
            })
            .await
            .expect("driver action channel stays open");

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("first commit arrives"),
            Some(block1.hash())
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second commit arrives"),
            Some(block2.hash())
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_treats_duplicate_commit_as_idempotent_and_keeps_draining() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let attempts = Arc::new(AtomicUsize::new(0));
        let verifier_attempts = attempts.clone();
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let attempts = verifier_attempts.clone();
            let commit_tx = commit_tx.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        let hash = block.hash();
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Err(zebra_consensus::RouterError::Block {
                                source: Box::new(zebra_consensus::VerifyBlockError::Block {
                                    source: zebra_consensus::BlockError::AlreadyInChain(
                                        hash,
                                        zebra_state::KnownBlock::BestChain,
                                    ),
                                }),
                            });
                        }
                        commit_tx
                            .send(hash)
                            .await
                            .expect("test commit receiver stays open");
                        Ok(hash)
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let read_requests = Arc::new(Mutex::new(Vec::new()));
        let read_requests_for_service = read_requests.clone();
        let block_hash = block.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let read_requests = read_requests_for_service.clone();
            async move {
                read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .push(request.clone());
                match request {
                    zebra_state::ReadRequest::FinalizedTip => {
                        Ok(zebra_state::ReadResponse::FinalizedTip(None))
                    }
                    zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                        block::Height(1),
                        block_hash,
                    )))),
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            block_sync,
            read_state,
            verifier,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                block: block.clone(),
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                block: block.clone(),
            })
            .await
            .expect("driver action channel stays open");

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second commit arrives after duplicate"),
            Some(block.hash())
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(
            read_requests
                .lock()
                .expect("test read request log is not poisoned")
                .iter()
                .any(|request| matches!(request, zebra_state::ReadRequest::Tip)),
            "duplicate commit should refresh block-sync frontiers from state"
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }
}
