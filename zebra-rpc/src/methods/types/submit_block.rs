//! Parameter and response types for the `submitblock` RPC.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

use zebra_chain::block::{self, Block};

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::GetBlockTemplateHandler;

/// Optional argument `jsonparametersobject` for `submitblock` RPC request
///
/// See the notes for the [`submit_block`](crate::methods::RpcServer::submit_block) RPC.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
pub struct SubmitBlockParameters {
    /// The workid for the block template. Currently unused.
    ///
    /// > If the server provided a workid, it MUST be included with submissions,
    ///
    /// Rationale:
    ///
    /// > If servers allow all mutations, it may be hard to identify which job it is based on.
    /// > While it may be possible to verify the submission by its content, it is much easier
    /// > to compare it to the job issued. It is very easy for the miner to keep track of this.
    /// > Therefore, using a "workid" is a very cheap solution to enable more mutations.
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0022#Rationale>
    #[serde(rename = "workid")]
    pub _work_id: Option<String>,
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubmitBlockErrorResponse {
    /// Block was already committed to the non-finalized or finalized state
    Duplicate,
    /// Block was already added to the state queue or channel, but not yet committed to the non-finalized state
    DuplicateInconclusive,
    /// Block was already committed to the non-finalized state, but not on the best chain
    Inconclusive,
    /// Block rejected as invalid
    Rejected,
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum SubmitBlockResponse {
    /// Block was not successfully submitted, return error
    ErrorResponse(SubmitBlockErrorResponse),
    /// Block successfully submitted, returns null
    Accepted,
}

impl Default for SubmitBlockResponse {
    fn default() -> Self {
        Self::ErrorResponse(SubmitBlockErrorResponse::Rejected)
    }
}

impl From<SubmitBlockErrorResponse> for SubmitBlockResponse {
    fn from(error_response: SubmitBlockErrorResponse) -> Self {
        Self::ErrorResponse(error_response)
    }
}

/// Up to four authenticated pending submissions, available to inbound peers for 90 seconds.
#[derive(Clone, Debug)]
pub struct SubmittedBlockCache {
    entries: Arc<watch::Sender<VecDeque<(block::Hash, Arc<Block>, Instant)>>>,
    pending: Arc<watch::Sender<Option<(block::Hash, block::Height)>>>,
}

impl Default for SubmittedBlockCache {
    fn default() -> Self {
        Self {
            entries: Arc::new(watch::Sender::new(VecDeque::new())),
            pending: Arc::new(watch::Sender::new(None)),
        }
    }
}

impl SubmittedBlockCache {
    /// Returns an unexpired body unless verification definitively rejected it.
    pub fn get(&self, hash: &block::Hash) -> Option<Arc<Block>> {
        let now = Instant::now();
        self.entries
            .borrow()
            .iter()
            .find(|(key, _, expires)| key == hash && *expires > now)
            .map(|(_, block, _)| block.clone())
    }

    /// Subscribes to pending advertisements, including the current one.
    pub fn subscribe(&self) -> watch::Receiver<Option<(block::Hash, block::Height)>> {
        let mut receiver = self.pending.subscribe();
        receiver.mark_changed();
        receiver
    }

    pub(crate) fn insert(&self, hash: block::Hash, height: block::Height, block: Arc<Block>) {
        // ponytail: scan four bodies in the shared snapshot; index only if the bound grows.
        let inserted = self.entries.send_if_modified(|entries| {
            let now = Instant::now();
            entries.retain(|(_, _, expires)| *expires > now);
            if entries.iter().any(|(key, _, _)| *key == hash) {
                return false;
            }
            if entries.len() == 4 {
                entries.pop_front();
            }
            entries.push_back((hash, block, now + Duration::from_secs(90)));
            true
        });
        if inserted {
            self.pending.send_replace(Some((hash, height)));
        }
    }

    /// Withdraws a definitively rejected body, without coupling its lifetime to any RPC caller.
    pub(crate) fn remove(&self, hash: block::Hash) {
        self.entries.send_modify(|entries| {
            entries.retain(|(key, _, _)| *key != hash);
        });
        self.pending.send_if_modified(|pending| {
            if pending.is_some_and(|(key, _)| key == hash) {
                *pending = None;
                true
            } else {
                false
            }
        });
    }
}

/// A submit block channel, used to inform the gossip task about mined blocks.
pub struct SubmitBlockChannel {
    /// The channel sender
    sender: mpsc::Sender<(block::Hash, block::Height)>,
    /// The channel receiver
    receiver: mpsc::Receiver<(block::Hash, block::Height)>,
}

impl SubmitBlockChannel {
    /// Creates a new submit block channel
    pub fn new() -> Self {
        /// How many unread messages the submit block channel should buffer before rejecting sends.
        ///
        /// This should be large enough to usually avoid rejecting sends. This channel is used by
        /// the block hash gossip task, which waits for a ready peer in the peer set while
        /// processing messages from this channel and could be much slower to gossip block hashes
        /// than it is to commit blocks and produce new block templates.
        const SUBMIT_BLOCK_CHANNEL_CAPACITY: usize = 10_000;

        let (sender, receiver) = mpsc::channel(SUBMIT_BLOCK_CHANNEL_CAPACITY);
        Self { sender, receiver }
    }

    /// Get the channel sender
    pub fn sender(&self) -> mpsc::Sender<(block::Hash, block::Height)> {
        self.sender.clone()
    }

    /// Get the channel receiver
    pub fn receiver(self) -> mpsc::Receiver<(block::Hash, block::Height)> {
        self.receiver
    }
}

impl Default for SubmitBlockChannel {
    fn default() -> Self {
        Self::new()
    }
}
