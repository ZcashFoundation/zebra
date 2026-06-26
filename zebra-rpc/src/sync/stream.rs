//! The untrusted-wire boundary for [`TrustedChainSync`]: the indexer gRPC `ChainStateChange`
//! stream, message decoding, (re)subscription, and the associated timeouts.

use std::{sync::Arc, time::Duration};

use tonic::{Status, Streaming};
use zebra_chain::serialization::BytesInDisplayOrder;
use zebra_state::SemanticallyVerifiedBlock;

use crate::indexer::{
    chain_state_change_message::Change, ChainStateChangeMessage, ChainStateChangeRequest,
};

use super::syncer::TrustedChainSync;

/// How long to wait between calls to `subscribe_to_chain_state_change` when it returns an error.
const POLL_DELAY: Duration = Duration::from_secs(5);

/// How long to wait for a message on a gRPC subscription stream before assuming the stream is dead
/// and re-subscribing.
///
/// Generous, because legitimate gaps between blocks/tip changes can be several minutes; this is a
/// backstop against a wedged connection that the keep-alive ping below doesn't catch. Re-subscribing
/// is cheap and resumes from the syncer's current chain tips, so an occasional false trigger during
/// a quiet period is harmless.
const STREAM_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long to wait to establish a gRPC subscription stream before assuming the request is wedged
/// and retrying. The subscription handshake should complete promptly.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

/// An actionable update decoded from the `ChainStateChange` stream.
pub(super) enum ChainStateUpdate {
    /// A new non-finalized block to commit.
    Block(SemanticallyVerifiedBlock),
    /// The finalized tip advanced; catch up to it and publish it.
    FinalizedTip,
}

impl TrustedChainSync {
    /// Returns the next actionable update, (re)subscribing and skipping malformed/empty messages.
    pub(super) async fn next_update(
        &mut self,
        stream: &mut Option<Streaming<ChainStateChangeMessage>>,
    ) -> ChainStateUpdate {
        loop {
            let Some(active) = stream.as_mut() else {
                *stream = self.resubscribe().await;
                continue;
            };

            let message = match tokio::time::timeout(STREAM_MESSAGE_TIMEOUT, active.message()).await
            {
                Ok(Ok(Some(message))) => message,
                Ok(Ok(None)) => {
                    tracing::warn!("chain state change stream ended unexpectedly");
                    *stream = None;
                    continue;
                }
                Ok(Err(err)) => {
                    tracing::warn!(?err, "error receiving chain state change");
                    *stream = None;
                    continue;
                }
                Err(_) => {
                    tracing::debug!("chain state change stream timed out, re-subscribing");
                    *stream = None;
                    continue;
                }
            };

            match message.change {
                Some(Change::NonFinalizedBlock(block)) => {
                    let Some((block, hash)) = block.decode() else {
                        tracing::warn!("received malformed non-finalized block message");
                        *stream = None;
                        continue;
                    };
                    let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), hash);
                    return ChainStateUpdate::Block(block);
                }
                Some(Change::FinalizedTipChange(_)) => return ChainStateUpdate::FinalizedTip,
                None => tracing::warn!("received empty chain state change message"),
            }
        }
    }

    /// Subscribes to the `ChainStateChange` stream, backing off on error.
    async fn resubscribe(&mut self) -> Option<Streaming<ChainStateChangeMessage>> {
        match self.subscribe_to_chain_state_change().await {
            Ok(stream) => Some(stream),
            Err(err) => {
                tracing::warn!(?err, "failed to subscribe to chain state changes");
                tokio::time::sleep(POLL_DELAY).await;
                None
            }
        }
    }

    /// Subscribes to the indexer's `ChainStateChange` stream, resuming from the tips we already have.
    ///
    /// Passing the tip hashes of every chain in our non-finalized state tells the server to stream
    /// only the blocks after them, instead of re-sending the whole non-finalized state on every
    /// (re)subscription. When the non-finalized state is empty, no tips are sent and the server
    /// streams every non-finalized block.
    async fn subscribe_to_chain_state_change(
        &mut self,
    ) -> Result<Streaming<ChainStateChangeMessage>, Status> {
        let request = ChainStateChangeRequest {
            chain_tip_hashes: self
                .non_finalized_state
                .chain_iter()
                .map(|c| c.non_finalized_tip_hash().bytes_in_display_order().to_vec())
                .collect(),
        };

        tokio::time::timeout(
            SUBSCRIBE_TIMEOUT,
            self.indexer_rpc_client.clone().chain_state_change(request),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("chain_state_change subscription timed out"))?
        .map(|response| response.into_inner())
    }
}
