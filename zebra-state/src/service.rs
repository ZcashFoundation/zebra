use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::future::{FutureExt, TryFutureExt};
use tokio::sync::oneshot;
use tower::{buffer::Buffer, util::BoxService, Service};
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

use crate::{BoxError, Config, MemoryState, Request, Response, SledState};

// todo: put this somewhere
pub struct QueuedBlock {
    pub block: Arc<Block>,
    // TODO: add these parameters when we can compute anchors.
    // sprout_anchor: sprout::tree::Root,
    // sapling_anchor: sapling::tree::Root,
    pub rsp_tx: oneshot::Sender<Result<block::Hash, BoxError>>,
}

struct StateService {
    /// Holds data relating to finalized chain state.
    sled: SledState,
    /// Holds data relating to non-finalized chain state.
    mem: MemoryState,
}

impl StateService {
    pub fn new(config: Config, network: Network) -> Self {
        let sled = SledState::new(&config, network);
        let mem = MemoryState {};
        Self { sled, mem }
    }
}

impl Service<Request> for StateService {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::CommitBlock { block } => unimplemented!(),
            Request::CommitFinalizedBlock { block } => {
                let (rsp_tx, rsp_rx) = oneshot::channel();

                self.sled.queue(QueuedBlock { block, rsp_tx });
                self.sled.process_queue();

                async move {
                    rsp_rx
                        .await
                        .expect("sender oneshot is not dropped")
                        .map(|hash| Response::Committed(hash))
                }
                .boxed()
            }
            Request::Depth(hash) => {
                // todo: handle in memory and sled
                self.sled
                    .depth(hash)
                    .map_ok(|depth| Response::Depth(depth))
                    .boxed()
            }
            Request::Tip => {
                // todo: handle in memory and sled
                self.sled.tip().map_ok(|tip| Response::Tip(tip)).boxed()
            }
            Request::BlockLocator => {
                // todo: handle in memory and sled
                self.sled
                    .block_locator()
                    .map_ok(|locator| Response::BlockLocator(locator))
                    .boxed()
            }
            Request::Transaction(hash) => unimplemented!(),
            Request::Block(hash_or_height) => {
                //todo: handle in memory and sled
                self.sled
                    .block(hash_or_height)
                    .map_ok(|block| Response::Block(block))
                    .boxed()
            }
        }
    }
}

/// Initialize a state service from the provided [`Config`].
///
/// Each `network` has its own separate sled database.
///
/// The resulting service is clonable, to provide shared access to a common chain
/// state. It's possible to construct multiple state services in the same
/// application (as long as they, e.g., use different storage locations), but
/// doing so is probably not what you want.
pub fn init(
    config: Config,
    network: Network,
) -> Buffer<BoxService<Request, Response, BoxError>, Request> {
    Buffer::new(BoxService::new(StateService::new(config, network)), 1)
}
