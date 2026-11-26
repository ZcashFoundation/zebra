//! Background checks of the transactions that the block verifier took from the mempool.

use std::time::Duration;

use tokio::sync::mpsc;
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block,
};

use crate::{error::TransactionError, transaction as tx, BoxError};

/// How long an audit waits for the transaction verifier.
///
/// The audited block is already committed, so its spent outputs are in the state. An audit that
/// doesn't get them in this time is inconclusive.
const AUDIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The number of audits that can wait while another audit runs.
///
/// A block whose audit would wait behind more audits isn't audited, so no backlog of audits builds
/// up.
pub(crate) const MAX_QUEUED_REUSE_AUDITS: usize = 1;

/// One transaction that a committed block took from the mempool, to verify again.
///
/// A block that solves a validated block proposal is committed with the proposal's validation
/// result, so it is audited with the proposal's transaction.
#[derive(Debug)]
pub(crate) struct ReuseAudit {
    /// The request that verifies the transaction in its block.
    pub(super) request: tx::BlockRequest,
    /// The block's hash, for logs.
    pub(super) block_hash: block::Hash,
    /// The miner fee that the block verifier took from the mempool's verification.
    pub(super) miner_fee: Amount<NonNegative>,
    /// The signature operation count that the block verifier took from the mempool's
    /// verification.
    pub(super) sigops: u32,
}

impl ReuseAudit {
    /// Returns this audit, which was sampled from a block proposal, for the block with
    /// `block_hash` that solves the proposal. That block has the proposal's transactions, height
    /// and time, so the audit's request stays the same.
    pub(crate) fn for_solved_block(self, block_hash: block::Hash) -> Self {
        Self { block_hash, ..self }
    }

    /// Verifies the transaction again, and returns the audit's result: `match`, `mismatch`, or
    /// `inconclusive`.
    ///
    /// The transaction verifier's caches can answer the proofs, signatures and scripts that the
    /// mempool already verified, so an audit mainly checks the transaction's spent outputs, its
    /// fee and signature operation count, and the rules that depend on its block.
    ///
    /// A mismatch means that the mempool and block transaction verifiers disagree, so the node can
    /// accept blocks that nodes which verify the transaction in full reject.
    async fn verify<V>(self, transaction_verifier: V) -> &'static str
    where
        V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
            + Send
            + Clone
            + 'static,
        V::Future: Send + 'static,
    {
        let Self {
            request,
            block_hash,
            miner_fee,
            sigops,
        } = self;
        let transaction_hash = request.transaction_hash;
        let height = request.height;

        match tokio::time::timeout(AUDIT_TIMEOUT, transaction_verifier.oneshot(request)).await {
            Ok(Ok(response))
                if response.miner_fee == Some(miner_fee) && response.sigops == sigops =>
            {
                "match"
            }
            Ok(Ok(response)) => {
                tracing::error!(
                    %block_hash,
                    ?height,
                    %transaction_hash,
                    ?miner_fee,
                    verified_miner_fee = ?response.miner_fee,
                    sigops,
                    verified_sigops = response.sigops,
                    "the block verifier took a different fee or signature operation count from \
                     the mempool than the transaction verifier finds for a transaction in a \
                     committed block",
                );
                "mismatch"
            }
            Ok(Err(error)) => match TransactionError::from(error) {
                // The state didn't find a spent output in time, or a service failed: neither is
                // a verdict about the transaction.
                TransactionError::TransparentInputNotFound
                | TransactionError::InternalDowncastError(_) => "inconclusive",
                error => {
                    tracing::error!(
                        %block_hash,
                        ?height,
                        %transaction_hash,
                        ?error,
                        "the transaction verifier rejects a transaction in a committed block that \
                         the block verifier took from the mempool",
                    );
                    "mismatch"
                }
            },
            Err(_elapsed) => "inconclusive",
        }
    }
}

/// Verifies one transaction that each committed block took from the mempool again, in the
/// background, and counts whether the transaction verifier agrees with the mempool's
/// verification.
///
/// Committed blocks are audited, including blocks that solve a validated block proposal, but
/// block proposals aren't. A task verifies the audits one at a time, so they add at most one
/// transaction verification at a time to the block transaction verifier's work. The default
/// handle has no task, and audits nothing.
#[derive(Clone, Debug, Default)]
pub(crate) struct Audits {
    /// Sends audits to the task that verifies them, or `None` if audits are off.
    sender: Option<mpsc::Sender<ReuseAudit>>,
}

impl Audits {
    /// Spawns the task that verifies the audits with `transaction_verifier`, and returns a handle
    /// that sends it audits.
    ///
    /// The task stops when every handle is dropped.
    pub(crate) fn spawn<V>(transaction_verifier: V) -> Self
    where
        V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
            + Send
            + Clone
            + 'static,
        V::Future: Send + 'static,
    {
        let (sender, mut receiver) = mpsc::channel::<ReuseAudit>(MAX_QUEUED_REUSE_AUDITS);
        tokio::spawn(
            async move {
                while let Some(audit) = receiver.recv().await {
                    let result = audit.verify(transaction_verifier.clone()).await;
                    metrics::counter!("zebra.consensus.block.mempool_reuse.audit", "result" => result)
                        .increment(1);
                }
            }
            .in_current_span(),
        );

        Self {
            sender: Some(sender),
        }
    }

    /// Sends `audit` to the task that verifies audits.
    ///
    /// The block's result never waits for its audit. If audits are off, or the task has stopped,
    /// the audit is dropped. If the task already has [`MAX_QUEUED_REUSE_AUDITS`] audits waiting,
    /// the audit is dropped and counted as skipped.
    pub(crate) fn send(&self, audit: Option<ReuseAudit>) {
        let (Some(sender), Some(audit)) = (&self.sender, audit) else {
            return;
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = sender.try_send(audit) {
            metrics::counter!("zebra.consensus.block.mempool_reuse.audit", "result" => "skipped")
                .increment(1);
        }
    }
}
