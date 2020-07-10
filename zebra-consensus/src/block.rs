//! Block verification and chain state updates for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (awaits a verified parent block)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

#[cfg(test)]
mod tests;

use chrono::{DateTime, Duration, Utc};
use futures_util::FutureExt;
use std::{
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::block::{Block, BlockHeaderHash};

/// Check if `block_header_time` is less than or equal to
/// 2 hours in the future, according to the node's local clock (`now`).
///
/// This is a non-deterministic rule, as clocks vary over time, and
/// between different nodes.
///
/// "In addition, a full validator MUST NOT accept blocks with nTime
/// more than two hours in the future according to its clock. This
/// is not strictly a consensus rule because it is nondeterministic,
/// and clock time varies between nodes. Also note that a block that
/// is rejected by this rule at a given point in time may later be
/// accepted."[S 7.5][7.5]
///
/// [7.5]: https://zips.z.cash/protocol/protocol.pdf#blockheader
pub(crate) fn node_time_check(
    block_header_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let two_hours_in_the_future = now
        .checked_add_signed(Duration::hours(2))
        .ok_or("overflow when calculating 2 hours in the future")?;

    if block_header_time <= two_hours_in_the_future {
        Ok(())
    } else {
        Err("block header time is more than 2 hours in the future".into())
    }
}

/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
pub(crate) fn coinbase_check(block: &Block) -> Result<(), Error> {
    if block.coinbase_height().is_some() {
        // No coinbase inputs in additional transactions allowed
        if block
            .transactions
            .iter()
            .skip(1)
            .any(|tx| tx.contains_coinbase_input())
        {
            Err("coinbase input found in additional transaction")?
        }
        Ok(())
    } else {
        Err("no coinbase transaction in block")?
    }
}

struct BlockVerifier<S> {
    /// The underlying `ZebraState`, possibly wrapped in other services.
    state_service: S,
}

/// The error type for the BlockVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The BlockVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<S> Service<Arc<Block>> for BlockVerifier<S>
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    type Response = BlockHeaderHash;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We don't expect the state to exert backpressure on verifier users,
        // so we don't need to call `state_service.poll_ready()` here.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report, handle errors from state_service.
        // TODO(teor):
        //   - handle chain reorgs
        //   - adjust state_service "unique block height" conditions
        let mut state_service = self.state_service.clone();

        async move {
            // Since errors cause an early exit, try to do the
            // quick checks first.

            let now = Utc::now();
            node_time_check(block.header.time, now)?;
            block.header.is_equihash_solution_valid()?;
            coinbase_check(block.as_ref())?;

            // `Tower::Buffer` requires a 1:1 relationship between `poll()`s
            // and `call()`s, because it reserves a buffer slot in each
            // `call()`.
            let add_block = state_service
                .ready_and()
                .await?
                .call(zebra_state::Request::AddBlock { block });

            match add_block.await? {
                zebra_state::Response::Added { hash } => Ok(hash),
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .boxed()
    }
}

/// Return a block verification service, using the provided state service.
///
/// The block verifier holds a state service of type `S`, used as context for
/// block validation and to which newly verified blocks will be committed. This
/// state is pluggable to allow for testing or instrumentation.
///
/// The returned type is opaque to allow instrumentation or other wrappers, but
/// can be boxed for storage. It is also `Clone` to allow sharing of a
/// verification service.
///
/// This function should be called only once for a particular state service (and
/// the result be shared) rather than constructing multiple verification services
/// backed by the same state layer.
//
// Only used by tests and other modules
#[allow(dead_code)]
pub fn init<S>(
    state_service: S,
) -> impl Service<
    Arc<Block>,
    Response = BlockHeaderHash,
    Error = Error,
    Future = impl Future<Output = Result<BlockHeaderHash, Error>>,
> + Send
       + Clone
       + 'static
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    Buffer::new(BlockVerifier { state_service }, 1)
}
