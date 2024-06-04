//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService via RPCs

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tower::BoxError;
use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{NonFinalizedState, SemanticallyVerifiedBlock, ZebraDb, MAX_BLOCK_REORG_HEIGHT};

use crate::methods::GetBlockHash;

/// Starts syncing non-finalized blocks from Zebra via the `getbestblockhash` and `getblock` RPC methods.
pub async fn sync_from_rpc(
    rpc_address: SocketAddr,
    finalized_state: ZebraDb,
    non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
) -> Result<(), BoxError> {
    let rpc_client = RpcRequestClient::new(rpc_address);
    let network = finalized_state.network();
    let mut non_finalized_state = NonFinalizedState::new(&network);
    let mut should_start_from_finalized_tip = false;

    loop {
        // Wait until the best block hash in Zebra is different from the tip hash in this read state
        let (best_block_hash, tip_height, tip_hash) = loop {
            let GetBlockHash(best_block_hash) = rpc_client
                .json_result_from_call("getbestblockhash", "[]")
                .await?;

            if should_start_from_finalized_tip {
                non_finalized_state = NonFinalizedState::new(&network);
                non_finalized_state_sender.send(non_finalized_state.clone())?;
                should_start_from_finalized_tip = false;
            }

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

            if best_block_hash != tip_hash {
                break (best_block_hash, tip_height, tip_hash);
            } else {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };

        loop {
            let raw_block: serde_json::Value = serde_json::from_str(
                &rpc_client
                    .text_from_call("getblock", format!(r#"["{}", 0]"#, tip_height.0))
                    .await?,
            )?;

            let Some(block_data) = raw_block["result"].as_str().map(hex::decode) else {
                // If there is no next block despite the best block hash changing, the chain hasn't grown, and so there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block past the finalized tip.
                should_start_from_finalized_tip = true;
                continue;
            };

            let block: Block = block_data?.zcash_deserialize_into()?;
            let block: SemanticallyVerifiedBlock = Arc::new(block).into();
            let parent_hash = block.block.header.previous_block_hash;
            if parent_hash != tip_hash {
                // If the next block's previous block hash doesn't match the expected hash, there must have
                // been a chain re-org/fork, and we can clear the non-finalized state and re-fetch every block
                // past the finalized tip.
                should_start_from_finalized_tip = true;
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
                    if block_hash == best_block_hash {
                        break;
                    }
                }
            }
        }
    }
}
