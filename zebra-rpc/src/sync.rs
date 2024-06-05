//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService via RPCs

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tower::BoxError;
use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{
    spawn_init_read_only, ChainTipBlock, ChainTipChange, ChainTipSender, CheckpointVerifiedBlock,
    LatestChainTip, NonFinalizedState, ReadStateService, SemanticallyVerifiedBlock, ZebraDb,
    MAX_BLOCK_REORG_HEIGHT,
};

use zebra_chain::diagnostic::task::WaitForPanics;

use crate::methods::{get_block_template_rpcs::types::hex_data::HexData, GetBlockHash};

/// Syncs non-finalized blocks in the best chain from a trusted Zebra node's RPC methods.
struct TrustedChainSync {
    /// RPC client for calling Zebra's RPC methods.
    rpc_client: RpcRequestClient,
    /// Information about the next block height to request and how it should be processed.
    cursor: SyncCursor,
    /// The read state service
    db: ZebraDb,
    /// The non-finalized state - currently only contains the best chain.
    non_finalized_state: NonFinalizedState,
    /// The chain tip sender for updating [`LatestChainTip`] and [`ChainTipChange`]
    chain_tip_sender: ChainTipSender,
    /// The non-finalized state sender, for updating the [`ReadStateService`] when the non-finalized best chain changes.
    non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
}

struct SyncCursor {
    /// The best chain tip height in this process.
    tip_height: Height,
    /// The best chain tip hash in this process.
    tip_hash: block::Hash,
    /// The best chain tip hash in the Zebra node.
    node_tip_hash: block::Hash,
}

impl SyncCursor {
    fn new(tip_height: Height, tip_hash: block::Hash, node_tip_hash: block::Hash) -> Self {
        Self {
            tip_height,
            tip_hash,
            node_tip_hash,
        }
    }
}

impl TrustedChainSync {
    /// Creates a new [`TrustedChainSync`] and starts syncing blocks from the node's non-finalized best chain.
    pub async fn spawn(
        rpc_address: SocketAddr,
        db: ZebraDb,
        non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
    ) -> (LatestChainTip, ChainTipChange, tokio::task::JoinHandle<()>) {
        // TODO: Spawn a tokio task to do this in and return a `JoinHandle`.
        let rpc_client = RpcRequestClient::new(rpc_address);
        let non_finalized_state = NonFinalizedState::new(&db.network());
        let (cursor, chain_tip) =
            initial_cursor_and_tip_block(&rpc_client, &non_finalized_state, &db).await;

        let (mut syncer, latest_chain_tip, chain_tip_change) = Self::new(
            rpc_client,
            cursor,
            chain_tip,
            db,
            non_finalized_state,
            non_finalized_state_sender,
        );

        let sync_task = tokio::spawn(async move {
            syncer.sync().await;
        });

        (latest_chain_tip, chain_tip_change, sync_task)
    }

    fn new(
        rpc_client: RpcRequestClient,
        cursor: SyncCursor,
        initial_tip: impl Into<ChainTipBlock>,
        db: ZebraDb,
        non_finalized_state: NonFinalizedState,
        non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
    ) -> (Self, LatestChainTip, ChainTipChange) {
        let (chain_tip_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(initial_tip.into(), &db.network());

        let syncer = Self {
            rpc_client,
            cursor,
            db,
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
        };

        (syncer, latest_chain_tip, chain_tip_change)
    }

    /// Starts syncing blocks from the node's non-finalized best chain.
    async fn sync(&mut self) {
        loop {
            let has_found_new_best_chain = self.sync_new_blocks().await;

            if has_found_new_best_chain {
                self.non_finalized_state = NonFinalizedState::new(&self.db.network());
                let _ = self
                    .non_finalized_state_sender
                    .send(self.non_finalized_state.clone());
            }

            // Wait until the best block hash in Zebra is different from the tip hash in this read state
            self.cursor = self.wait_for_new_blocks().await;
        }
    }

    async fn sync_new_blocks(&mut self) -> bool {
        loop {
            // TODO:
            // - Impl methods for `getbestblockhash` and `getblock` on RpcRequestClient
            // - Move non-finalized state resets below this loop, also

            // TODO: Move all this except the `.filter()` call to a method on RpcRequestClient
            let Some(block) = self
                .rpc_client
                .json_result_from_call(
                    "getblock",
                    format!(r#"["{}", 0]"#, self.cursor.tip_height.0),
                )
                .await
                // If we fail to get a block for any reason, we assume the block is missing and the chain hasn't grown, so there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block past the finalized tip.
                // TODO: Check for the MISSING_BLOCK_ERROR_CODE?
                .ok()
                // It should always deserialize successfully, but this resets the non-finalized state if it somehow fails
                // TODO: Log a warning, or, unrelated to that, panic instead if this should never happen? Could be a bad message tho, warning sounds fine
                .and_then(|HexData(raw_block)| raw_block.zcash_deserialize_into::<Block>().ok())
                .map(Arc::new)
                .map(SemanticallyVerifiedBlock::from)
                // If the next block's previous block hash doesn't match the expected hash, there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block
                // past the finalized tip.
                .filter(|block| block.block.header.previous_block_hash == self.cursor.tip_hash)
            else {
                break true;
            };

            let parent_hash = block.block.header.previous_block_hash;
            if parent_hash != self.cursor.tip_hash {
                break true;
            }

            let block_hash = block.hash;
            let block_height = block.height;
            let commit_result = if self.non_finalized_state.chain_count() == 0 {
                self.non_finalized_state
                    .commit_new_chain(block.clone(), &self.db)
            } else {
                self.non_finalized_state
                    .commit_block(block.clone(), &self.db)
            };

            if let Err(error) = commit_result {
                tracing::warn!(
                    ?error,
                    ?block_hash,
                    "failed to commit block to non-finalized state"
                );
                continue;
            }

            self.update_channels(block);

            while self
                .non_finalized_state
                .best_chain_len()
                .expect("just successfully inserted a non-finalized block above")
                > MAX_BLOCK_REORG_HEIGHT
            {
                tracing::trace!("finalizing block past the reorg limit");
                self.non_finalized_state.finalize();
            }

            let _ = self
                .non_finalized_state_sender
                .send(self.non_finalized_state.clone());

            self.cursor.tip_height = block_height;
            self.cursor.tip_hash = block_hash;

            // If the block hash matches the output from the `getbestblockhash` RPC method, we can wait until
            // the best block hash changes to get the next block.
            if block_hash == self.cursor.node_tip_hash {
                break false;
            }
        }
    }

    /// Polls `getbestblockhash` RPC method until there are new blocks in the Zebra node's non-finalized state.
    async fn wait_for_new_blocks(&self) -> SyncCursor {
        // Wait until the best block hash in Zebra is different from the tip hash in this read state
        loop {
            let Some(node_block_hash) = self.rpc_client.get_best_block_hash().await else {
                // Wait until the genesis block has been committed.
                // TODO:
                // - Move durations to constants
                // - Add logs
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };

            let (tip_height, tip_hash) = if let Some(tip) = self.non_finalized_state.best_tip() {
                tip
            } else {
                let db = self.db.clone();
                tokio::task::spawn_blocking(move || db.tip())
                    .wait_for_panics()
                    .await
                    .expect("checked for genesis block above")
            };

            if node_block_hash != tip_hash {
                break SyncCursor::new(tip_height, tip_hash, node_block_hash);
            } else {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    /// Sends the new chain tip and non-finalized state to the latest chain channels.
    fn update_channels(&mut self, best_tip: impl Into<ChainTipBlock>) {
        // If the final receiver was just dropped, ignore the error.
        let _ = self
            .non_finalized_state_sender
            .send(self.non_finalized_state.clone());

        self.chain_tip_sender
            .set_best_non_finalized_tip(best_tip.into());
    }
}

async fn initial_cursor_and_tip_block(
    rpc_client: &RpcRequestClient,
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
) -> (SyncCursor, ChainTipBlock) {
    // Wait until the best block hash in Zebra is different from the tip hash in this read state
    loop {
        let Some(node_block_hash) = rpc_client.get_best_block_hash().await else {
            // Wait until the genesis block has been committed.
            // TODO:
            // - Move durations to constants
            // - Add logs
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };

        let chain_tip: ChainTipBlock = if let Some(tip_block) = non_finalized_state.best_tip_block()
        {
            tip_block.clone().into()
        } else if let Some(tip_block) = {
            let db = db.clone();
            tokio::task::spawn_blocking(move || db.tip_block())
                .wait_for_panics()
                .await
        } {
            CheckpointVerifiedBlock::from(tip_block).into()
        } else {
            // If there is no genesis block, wait 200ms and try again.
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };

        if node_block_hash != chain_tip.hash {
            let cursor = SyncCursor::new(chain_tip.height, chain_tip.hash, node_block_hash);
            break (cursor, chain_tip);
        } else {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Accepts a [zebra-state configuration](zebra_state::Config), a [`Network`], and
/// the [`SocketAddr`] of a Zebra node's RPC server.
///
/// Initializes a [`ReadStateService`] and a [`TrustedChainSync`] to update the
/// non-finalized best chain and the latest chain tip.
///
/// Returns a [`ReadStateService`], [`LatestChainTip`], [`ChainTipChange`], and
/// a [`JoinHandle`](tokio::task::JoinHandle) for the sync task.
pub fn init_read_state_with_syncer(
    config: zebra_state::Config,
    network: &Network,
    rpc_address: SocketAddr,
) -> tokio::task::JoinHandle<
    Result<
        (
            ReadStateService,
            LatestChainTip,
            ChainTipChange,
            tokio::task::JoinHandle<()>,
        ),
        BoxError,
    >,
> {
    // TODO: Return an error or panic `if config.ephemeral == true`? (It'll panic anyway but it could be useful to say it's because the state is ephemeral).
    let network = network.clone();
    tokio::spawn(async move {
        let (read_state, db, non_finalized_state_sender) =
            spawn_init_read_only(config, &network).await?;
        let (latest_chain_tip, chain_tip_change, sync_task) =
            TrustedChainSync::spawn(rpc_address, db, non_finalized_state_sender).await;
        Ok((read_state, latest_chain_tip, chain_tip_change, sync_task))
    })
}

trait SyncerRpcMethods {
    async fn get_best_block_hash(&self) -> Option<block::Hash>;
    async fn get_block(&self, height: block::Height) -> Option<Arc<Block>>;
}

impl SyncerRpcMethods for RpcRequestClient {
    async fn get_best_block_hash(&self) -> Option<block::Hash> {
        self.json_result_from_call("getbestblockhash", "[]")
            .await
            .map(|GetBlockHash(hash)| hash)
            .ok()
    }

    async fn get_block(&self, Height(height): Height) -> Option<Arc<Block>> {
        self.json_result_from_call("getblock", format!(r#"["{}", 0]"#, height))
            .await
            // If we fail to get a block for any reason, we assume the block is missing and the chain hasn't grown, so there must have
            // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block past the finalized tip.
            // TODO: Check for the MISSING_BLOCK_ERROR_CODE?
            .ok()
            // It should always deserialize successfully, but this resets the non-finalized state if it somehow fails
            // TODO: Log a warning, or, unrelated to that, panic instead if this should never happen? Could be a bad message tho, warning sounds fine
            .and_then(|HexData(raw_block)| raw_block.zcash_deserialize_into::<Block>().ok())
            .map(Arc::new)
    }
}

/// Starts syncing non-finalized blocks from Zebra via the `getbestblockhash` and `getblock` RPC methods.
pub async fn sync_from_rpc(
    rpc_address: SocketAddr,
    finalized_state: ZebraDb,
    non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
) -> Result<(), BoxError> {
    let rpc_client = RpcRequestClient::new(rpc_address);
    let network = finalized_state.network();
    let mut non_finalized_state = NonFinalizedState::new(&network);

    loop {
        // Wait until the best block hash in Zebra is different from the tip hash in this read state
        let SyncPosition {
            current_tip_height,
            current_tip_hash,
            node_tip_hash,
        } = wait_for_new_blocks(&rpc_client, &finalized_state, &non_finalized_state).await?;

        loop {
            // TODO:
            // - Impl methods for `getbestblockhash` and `getblock` on RpcRequestClient
            // - Move non-finalized state resets below this loop, also

            // TODO: Move all this except the `.filter()` call to a method on RpcRequestClient
            let Some(block) = rpc_client
                .json_result_from_call("getblock", format!(r#"["{}", 0]"#, current_tip_height.0))
                .await
                // If we fail to get a block for any reason, we assume the block is missing and the chain hasn't grown, so there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block past the finalized tip.
                // TODO: Check for the MISSING_BLOCK_ERROR_CODE?
                .ok()
                // It should always deserialize successfully, but this resets the non-finalized state if it somehow fails
                // TODO: Log a warning, or, unrelated to that, panic instead if this should never happen? Could be a bad message tho, warning sounds fine
                .and_then(|HexData(raw_block)| raw_block.zcash_deserialize_into::<Block>().ok())
                .map(Arc::new)
                .map(SemanticallyVerifiedBlock::from)
                // If the next block's previous block hash doesn't match the expected hash, there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block
                // past the finalized tip.
                .filter(|block| block.block.header.previous_block_hash == current_tip_hash)
            else {
                non_finalized_state = NonFinalizedState::new(&finalized_state.network());
                non_finalized_state_sender.send(non_finalized_state.clone())?;
                continue;
            };

            let parent_hash = block.block.header.previous_block_hash;
            if parent_hash != current_tip_hash {
                non_finalized_state = NonFinalizedState::new(&finalized_state.network());
                non_finalized_state_sender.send(non_finalized_state.clone())?;
                continue;
            } else {
                let block_hash = block.hash;

                let finalized_tip_hash = {
                    let finalized_state = finalized_state.clone();
                    tokio::task::spawn_blocking(move || finalized_state.finalized_tip_hash())
                        .await?
                };

                let commit_result = if finalized_tip_hash == parent_hash {
                    non_finalized_state.commit_new_chain(block, &finalized_state)
                } else {
                    non_finalized_state.commit_block(block, &finalized_state)
                };

                if let Err(error) = commit_result {
                    tracing::warn!(?error, "failed to commit block to non-finalized state");
                    continue;
                }

                while non_finalized_state
                    .best_chain_len()
                    .expect("just successfully inserted a non-finalized block above")
                    > MAX_BLOCK_REORG_HEIGHT
                {
                    tracing::trace!("finalizing block past the reorg limit");
                    non_finalized_state.finalize();
                }

                if commit_result.is_ok() {
                    let _ = non_finalized_state_sender.send(non_finalized_state.clone());
                    // If the block hash matches the output from the `getbestblockhash` RPC method, we can wait until
                    // the best block hash changes to get the next block.
                    if block_hash == node_tip_hash {
                        break;
                    }
                }
            }
        }
    }
}

struct SyncPosition {
    current_tip_height: Height,
    current_tip_hash: block::Hash,
    node_tip_hash: block::Hash,
}

impl SyncPosition {
    fn new(
        current_tip_height: Height,
        current_tip_hash: block::Hash,
        node_tip_hash: block::Hash,
    ) -> Self {
        Self {
            current_tip_hash,
            current_tip_height,
            node_tip_hash,
        }
    }
}

/// Polls `getbestblockhash` RPC method until there are new blocks in the Zebra node's non-finalized state.
async fn wait_for_new_blocks(
    rpc_client: &RpcRequestClient,
    finalized_state: &ZebraDb,
    non_finalized_state: &NonFinalizedState,
) -> Result<SyncPosition, BoxError> {
    // Wait until the best block hash in Zebra is different from the tip hash in this read state
    loop {
        let GetBlockHash(node_block_hash) = rpc_client
            .json_result_from_call("getbestblockhash", "[]")
            .await?;

        let (tip_height, tip_hash) = if let Some(tip) = non_finalized_state.best_tip() {
            tip
        } else if let Some(tip) = {
            let finalized_state = finalized_state.clone();
            tokio::task::spawn_blocking(move || finalized_state.tip()).await?
        } {
            tip
        } else {
            // If there is no genesis block, wait 200ms and try again.
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };

        if node_block_hash != tip_hash {
            break Ok(SyncPosition::new(tip_height, tip_hash, node_block_hash));
        } else {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
