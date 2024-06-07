//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService and updating `ChainTipSender` via RPCs

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::task::JoinHandle;
use tower::BoxError;
use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{
    spawn_init_read_only, ChainTipBlock, ChainTipChange, ChainTipSender, LatestChainTip,
    NonFinalizedState, ReadStateService, SemanticallyVerifiedBlock, ZebraDb,
    MAX_BLOCK_REORG_HEIGHT,
};

use zebra_chain::diagnostic::task::WaitForPanics;

use crate::{
    constants::MISSING_BLOCK_ERROR_CODE,
    methods::{get_block_template_rpcs::types::hex_data::HexData, GetBlockHash},
};

/// How long to wait between calls to `getbestblockhash` when it returns an error or the block hash
/// of the current chain tip in the process that's syncing blocks from Zebra.
const POLL_DELAY: Duration = Duration::from_millis(200);

/// Syncs non-finalized blocks in the best chain from a trusted Zebra node's RPC methods.
#[derive(Debug)]
struct TrustedChainSync {
    /// RPC client for calling Zebra's RPC methods.
    rpc_client: RpcRequestClient,
    /// The read state service
    db: ZebraDb,
    /// The non-finalized state - currently only contains the best chain.
    non_finalized_state: NonFinalizedState,
    /// The chain tip sender for updating [`LatestChainTip`] and [`ChainTipChange`]
    chain_tip_sender: ChainTipSender,
    /// The non-finalized state sender, for updating the [`ReadStateService`] when the non-finalized best chain changes.
    non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
}

#[derive(Debug)]
enum NewChainTip {
    /// Information about the next block height to request and how it should be processed.
    Cursor(SyncCursor),
    /// The latest finalized tip.
    Block(Arc<Block>),
}

#[derive(Debug)]
struct SyncCursor {
    /// The best chain tip height in this process.
    tip_height: Height,
    /// The best chain tip hash in this process.
    tip_hash: block::Hash,
    /// The best chain tip hash in the Zebra node.
    target_tip_hash: block::Hash,
}

impl SyncCursor {
    fn new(tip_height: Height, tip_hash: block::Hash, target_tip_hash: block::Hash) -> Self {
        Self {
            tip_height,
            tip_hash,
            target_tip_hash,
        }
    }
}

impl TrustedChainSync {
    /// Creates a new [`TrustedChainSync`] and starts syncing blocks from the node's non-finalized best chain.
    pub async fn spawn(
        rpc_address: SocketAddr,
        db: ZebraDb,
        non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
    ) -> (LatestChainTip, ChainTipChange, JoinHandle<()>) {
        let rpc_client = RpcRequestClient::new(rpc_address);
        let non_finalized_state = NonFinalizedState::new(&db.network());
        let (chain_tip_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &db.network());

        let mut syncer = Self {
            rpc_client,
            db,
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
        };

        let sync_task = tokio::spawn(async move {
            syncer.sync().await;
        });

        (latest_chain_tip, chain_tip_change, sync_task)
    }

    /// Starts syncing blocks from the node's non-finalized best chain.
    async fn sync(&mut self) {
        loop {
            // Wait until the best block hash in Zebra is different from the tip hash in this read state
            match self.wait_for_chain_tip_change().await {
                NewChainTip::Cursor(cursor) => {
                    self.sync_new_blocks(cursor).await;
                }

                NewChainTip::Block(block) => update_channels(
                    block,
                    &self.non_finalized_state,
                    &mut self.non_finalized_state_sender,
                    &mut self.chain_tip_sender,
                ),
            }
        }
    }

    /// Accepts a [`SyncCursor`] and gets blocks after the current chain tip until reaching the target block hash in the cursor.
    ///
    /// Clears the non-finalized state and sets the finalized tip as the current chain tip if
    /// the `getblock` RPC method returns the `MISSING_BLOCK_ERROR_CODE` before this function reaches
    /// the target block hash.
    async fn sync_new_blocks(&mut self, mut cursor: SyncCursor) {
        let mut consecutive_unexpected_error_count = 0;

        let has_found_new_best_chain = loop {
            let block = match self.rpc_client.get_block(cursor.tip_height).await {
                Ok(Some(block)) if block.header.previous_block_hash == cursor.tip_hash => {
                    SemanticallyVerifiedBlock::from(block)
                }
                // If the next block's previous block hash doesn't match the expected hash, or if the block is missing
                // before this function reaches the target block hash, there was likely a chain re-org/fork, and
                // we can clear the non-finalized state and re-fetch every block past the finalized tip.
                Ok(_) => break true,
                // If there was an unexpected error, retry 4 more times before returning early.
                // TODO: Propagate this error up and exit the spawned sync task with the error if the RPC server is down for too long.
                Err(err) => {
                    tracing::warn!(
                        ?consecutive_unexpected_error_count,
                        ?cursor,
                        ?err,
                        "encountered an unexpected error while calling getblock method"
                    );

                    if consecutive_unexpected_error_count >= 5 {
                        break false;
                    } else {
                        consecutive_unexpected_error_count += 1;
                        continue;
                    };
                }
            };

            consecutive_unexpected_error_count = 0;

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

            while self
                .non_finalized_state
                .best_chain_len()
                .expect("just successfully inserted a non-finalized block above")
                > MAX_BLOCK_REORG_HEIGHT
            {
                tracing::trace!("finalizing block past the reorg limit");
                self.non_finalized_state.finalize();
            }

            update_channels(
                block,
                &self.non_finalized_state,
                &mut self.non_finalized_state_sender,
                &mut self.chain_tip_sender,
            );

            cursor.tip_height = block_height;
            cursor.tip_hash = block_hash;

            // If the block hash matches the output from the `getbestblockhash` RPC method, we can wait until
            // the best block hash changes to get the next block.
            if block_hash == cursor.target_tip_hash {
                break false;
            }
        };

        if has_found_new_best_chain {
            self.non_finalized_state = NonFinalizedState::new(&self.db.network());
            let db = self.db.clone();
            let finalized_tip = tokio::task::spawn_blocking(move || {
                db.tip_block().expect("should have genesis block")
            })
            .wait_for_panics()
            .await;

            update_channels(
                finalized_tip,
                &self.non_finalized_state,
                &mut self.non_finalized_state_sender,
                &mut self.chain_tip_sender,
            );
        }
    }

    /// Polls `getbestblockhash` RPC method until there are new blocks in the Zebra node's non-finalized state.
    async fn wait_for_chain_tip_change(&self) -> NewChainTip {
        let (tip_height, tip_hash) = if let Some(tip) = self.non_finalized_state.best_tip() {
            tip
        } else {
            let db = self.db.clone();
            tokio::task::spawn_blocking(move || db.tip())
                .wait_for_panics()
                .await
                .expect("checked for genesis block above")
        };

        // Wait until the best block hash in Zebra is different from the tip hash in this read state
        loop {
            let Some(target_tip_hash) = self.rpc_client.get_best_block_hash().await else {
                // Wait until the genesis block has been committed.
                tokio::time::sleep(POLL_DELAY).await;
                continue;
            };

            if target_tip_hash != tip_hash {
                let cursor =
                    NewChainTip::Cursor(SyncCursor::new(tip_height, tip_hash, target_tip_hash));

                // Check if there's are blocks in the non-finalized state, or that
                // the node tip hash is different from our finalized tip hash before returning
                // a cursor for syncing blocks via the `getblock` RPC.
                if self.non_finalized_state.chain_count() != 0 {
                    break cursor;
                }

                let db = self.db.clone();
                break tokio::task::spawn_blocking(move || {
                    if db.finalized_tip_hash() != target_tip_hash {
                        cursor
                    } else {
                        NewChainTip::Block(db.tip_block().unwrap())
                    }
                })
                .wait_for_panics()
                .await;
            }

            tokio::time::sleep(POLL_DELAY).await;
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
    let network = network.clone();
    tokio::spawn(async move {
        if config.ephemeral {
            return Err("standalone read state service cannot be used with ephemeral state".into());
        }

        let (read_state, db, non_finalized_state_sender) =
            spawn_init_read_only(config, &network).await?;
        let (latest_chain_tip, chain_tip_change, sync_task) =
            TrustedChainSync::spawn(rpc_address, db, non_finalized_state_sender).await;
        Ok((read_state, latest_chain_tip, chain_tip_change, sync_task))
    })
}

trait SyncerRpcMethods {
    async fn get_best_block_hash(&self) -> Option<block::Hash>;
    async fn get_block(&self, height: block::Height) -> Result<Option<Arc<Block>>, BoxError>;
}

impl SyncerRpcMethods for RpcRequestClient {
    async fn get_best_block_hash(&self) -> Option<block::Hash> {
        self.json_result_from_call("getbestblockhash", "[]")
            .await
            .map(|GetBlockHash(hash)| hash)
            .ok()
    }

    async fn get_block(&self, Height(height): Height) -> Result<Option<Arc<Block>>, BoxError> {
        match self
            .json_result_from_call("getblock", format!(r#"["{}", 0]"#, height))
            .await
        {
            Ok(HexData(raw_block)) => {
                let block = raw_block.zcash_deserialize_into::<Block>()?;
                Ok(Some(Arc::new(block)))
            }
            Err(err)
                if err
                    .downcast_ref::<jsonrpc_core::Error>()
                    .is_some_and(|err| err.code == MISSING_BLOCK_ERROR_CODE) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

/// Sends the new chain tip and non-finalized state to the latest chain channels.
fn update_channels(
    best_tip: impl Into<ChainTipBlock>,
    non_finalized_state: &NonFinalizedState,
    non_finalized_state_sender: &mut tokio::sync::watch::Sender<NonFinalizedState>,
    chain_tip_sender: &mut ChainTipSender,
) {
    // If the final receiver was just dropped, ignore the error.
    let _ = non_finalized_state_sender.send(non_finalized_state.clone());
    chain_tip_sender.set_best_non_finalized_tip(best_tip.into());
}
