//! Verification of a block's transactions.

use std::{collections::HashMap, sync::Arc};

use futures::stream::FuturesUnordered;
use tower::{Service, ServiceExt};

use zebra_chain::{
    amount::{self, Amount, NonNegative},
    block::{self, Block},
    transaction, transparent,
};

use crate::{transaction as tx, BoxError};

use super::{
    mempool_reuse::{Candidates, ReuseRules},
    VerifyBlockError,
};

/// The transactions of a block that passed the block checks, and what the block verifier needs to
/// verify them.
pub(super) struct BlockTransactions<'a, V> {
    /// The verifier of the transactions that the block verifier doesn't take from the mempool.
    pub(super) transaction_verifier: V,
    /// The block.
    pub(super) block: &'a Block,
    /// The mined IDs of the block's transactions, in block order.
    pub(super) transaction_hashes: &'a [transaction::Hash],
    /// The outputs that the block's transactions create.
    pub(super) known_utxos: &'a Arc<HashMap<transparent::OutPoint, transparent::OrderedUtxo>>,
    /// The block's height.
    pub(super) height: block::Height,
    /// The mempool's verifications of the block's transactions.
    pub(super) candidates: &'a mut Candidates,
    /// The rules that the block verifier checks again for each transaction it takes from the
    /// mempool.
    pub(super) rules: &'a ReuseRules,
}

impl<V> BlockTransactions<'_, V>
where
    V: Service<tx::BlockRequest, Response = tx::BlockResponse, Error = BoxError>
        + Send
        + Clone
        + 'static,
    V::Future: Send + 'static,
{
    /// Verifies the block's transactions, apart from the ones that it takes from the mempool,
    /// and returns the block's signature operation count and miner fees.
    pub(super) async fn verify(
        self,
    ) -> Result<(u32, Result<Amount<NonNegative>, amount::Error>), VerifyBlockError> {
        let Self {
            mut transaction_verifier,
            block,
            transaction_hashes,
            known_utxos,
            height,
            candidates,
            rules,
        } = self;

        // Send transactions to the transaction verifier to be checked
        let mut async_checks = FuturesUnordered::new();
        // Saturating: the counts of reused transactions come from the mempool, and a wrong
        // count must not wrap the total below the limit.
        let mut sigops: u32 = 0;
        let mut block_miner_fees = Ok(Amount::zero());

        for (&transaction_hash, transaction) in
            transaction_hashes.iter().zip(block.transactions.iter())
        {
            if let Some(cached) = candidates.take(transaction, transaction_hash, rules)? {
                sigops = sigops.saturating_add(cached.block_sigop_count());
                block_miner_fees += cached.miner_fee;
                // Taking a transaction from the mempool doesn't wait for anything, so let other
                // tasks run when this one has used its budget.
                tokio::task::coop::consume_budget().await;
                continue;
            }
            let rsp = transaction_verifier
                .ready()
                .await
                .expect("transaction verifier is always ready")
                .call(tx::BlockRequest {
                    transaction_hash,
                    transaction: transaction.clone(),
                    known_utxos: known_utxos.clone(),
                    height,
                    time: block.header.time,
                });
            async_checks.push(rsp);
        }
        tracing::trace!(len = async_checks.len(), "built async tx checks");

        // Get the transaction results back from the transaction verifier.

        // Sum up some block totals from the transaction responses.
        use futures::StreamExt;
        while let Some(result) = async_checks.next().await {
            tracing::trace!(?result, remaining = async_checks.len());
            let response = result
                .map_err(Into::into)
                .map_err(VerifyBlockError::Transaction)?;

            sigops = sigops.saturating_add(response.sigops);

            // Coinbase transactions consume the miner fee,
            // so they don't add any value to the block's total miner fee.
            if let Some(miner_fee) = response.miner_fee {
                block_miner_fees += miner_fee;
            }
        }

        Ok((sigops, block_miner_fees))
    }
}
