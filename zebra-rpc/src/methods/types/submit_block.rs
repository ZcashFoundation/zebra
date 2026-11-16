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

/// Optional ignored argument `jsonparametersobject` for the `submitblock` RPC.
///
/// Any JSON value is accepted. A string `workid` in an object is retained but unused.
///
/// See the notes for the [`submit_block`](crate::methods::RpcServer::submit_block) RPC.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(from = "serde_json::Value")]
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
    pub _work_id: Option<String>,
}

impl From<serde_json::Value> for SubmitBlockParameters {
    fn from(mut value: serde_json::Value) -> Self {
        Self {
            _work_id: match value.get_mut("workid") {
                Some(serde_json::Value::String(work_id)) => Some(std::mem::take(work_id)),
                _ => None,
            },
        }
    }
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, strum_macros::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
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

impl SubmitBlockResponse {
    /// Records the bounded submission result without counting operational RPC errors as rejections.
    pub(in crate::methods) fn with_metrics(self) -> Self {
        let outcome: &'static str = match &self {
            Self::Accepted => "accepted",
            Self::ErrorResponse(error) => error.into(),
        };
        metrics::counter!("rpc.submitblock.outcomes.total", "outcome" => outcome).increment(1);
        self
    }
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

/// Up to four locally validated pending submissions, available to inbound peers for 90 seconds.
#[derive(Clone, Debug)]
pub struct SubmittedBlockCache {
    entries: Arc<watch::Sender<VecDeque<(block::Hash, block::Height, Arc<Block>, Instant)>>>,
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
    /// Returns an unexpired, validated body unless its commit definitively failed.
    pub fn get(&self, hash: &block::Hash) -> Option<Arc<Block>> {
        let now = Instant::now();
        self.entries
            .borrow()
            .iter()
            .find(|(key, _, _, expires)| key == hash && *expires > now)
            .map(|(_, _, block, _)| block.clone())
    }

    /// Subscribes to pending advertisements, including the current one.
    pub fn subscribe(&self) -> watch::Receiver<Option<(block::Hash, block::Height)>> {
        let mut receiver = self.pending.subscribe();
        receiver.mark_changed();
        receiver
    }

    pub(crate) fn insert(&self, hash: block::Hash, height: block::Height, block: Arc<Block>) {
        // ponytail: scan four bodies in the shared snapshot; index only if the bound grows.
        self.entries.send_if_modified(|entries| {
            let now = Instant::now();
            entries.retain(|(_, _, _, expires)| *expires > now);
            if entries.iter().any(|(key, _, _, _)| *key == hash) {
                return false;
            }
            if entries.len() == 4 {
                entries.pop_front();
            }
            entries.push_back((hash, height, block, now + Duration::from_secs(90)));
            // Publish under the entries lock so concurrent removals cannot leave a stale advert.
            self.pending.send_replace(Some((hash, height)));
            true
        });
    }

    /// Withdraws a definitively rejected body, without coupling its lifetime to any RPC caller.
    pub(crate) fn remove(&self, hash: block::Hash) {
        self.entries.send_modify(|entries| {
            let now = Instant::now();
            entries.retain(|(key, _, _, expires)| *key != hash && *expires > now);
            self.pending.send_if_modified(|pending| {
                if pending.is_some_and(|(key, _)| key == hash) {
                    *pending = entries.back().map(|(key, height, _, _)| (*key, *height));
                    true
                } else {
                    false
                }
            });
        });
    }
}

/// Bounded state-owned committed notifications, independent of the submitting RPC's lifetime.
pub struct SubmitBlockChannel {
    /// The channel sender
    sender: mpsc::Sender<(block::Hash, block::Height)>,
    /// The channel receiver
    receiver: mpsc::Receiver<(block::Hash, block::Height)>,
}

impl SubmitBlockChannel {
    /// Creates a new submit block channel
    pub fn new() -> Self {
        /// Maximum queued committed notifications, before commit-ready mined requests fail admission.
        ///
        /// State reserves without waiting immediately before mutation, not while waiting for an
        /// absent parent. A full or closed channel leaves the block uncommitted and ordinary state
        /// writes unaffected. Once reserved, a successful commit inserts its notification before
        /// replying; insertion is not a guarantee of gossip consumption or peer delivery.
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
