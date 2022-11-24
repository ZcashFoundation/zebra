//! The [ZIP-317 block production algorithm](https://zips.z.cash/zip-0317#block-production).
//!
//! This is recommended algorithm, so these calculations are not consensus-critical,
//! or standardised across node implementations:
//! > it is sufficient to use floating point arithmetic to calculate the argument to `floor`
//! > when computing `size_target`, since there is no consensus requirement for this to be
//! > exactly the same between implementations.

use jsonrpc_core::{Error, ErrorCode, Result};
use tower::{Service, ServiceExt};

use zebra_chain::transaction::VerifiedUnminedTx;
use zebra_node_services::mempool;

/// The weight cap for ZIP-317 block production.
///
/// We use `f32` for efficient calculations when the mempool holds a large number of transactions.
const WEIGHT_CAP: f32 = 4.0;

/// Selects mempool transactions for block production according to [ZIP-317].
///
/// Returns selected transactions from the `mempool`, or an error if the mempool has failed.
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub async fn select_mempool_transactions<Mempool>(
    mempool: Mempool,
) -> Result<Vec<VerifiedUnminedTx>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
{
    let mempool_transactions = fetch_mempool_transactions(mempool).await?;

    // TODO: ZIP-317

    Ok(mempool_transactions)
}

async fn fetch_mempool_transactions<Mempool>(mempool: Mempool) -> Result<Vec<VerifiedUnminedTx>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
{
    let response = mempool
        .oneshot(mempool::Request::FullTransactions)
        .await
        .map_err(|error| Error {
            code: ErrorCode::ServerError(0),
            message: error.to_string(),
            data: None,
        })?;

    if let mempool::Response::FullTransactions(transactions) = response {
        Ok(transactions)
    } else {
        unreachable!("unmatched response to a mempool::FullTransactions request")
    }
}
