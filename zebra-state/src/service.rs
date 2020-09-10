use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::{FutureExt, TryFutureExt};
use tower::{buffer::Buffer, util::BoxService, Service};
use zebra_chain::parameters::Network;

use crate::{BoxError, Config, HashOrHeight, MemoryState, Request, Response, SledState};

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
                let rsp = self
                    .sled
                    .commit_finalized(block)
                    .map(|hash| Response::Committed(hash));

                async move { rsp }.boxed()
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
            Request::Block(HashOrHeight::Hash(hash)) => unimplemented!(),
            Request::Block(HashOrHeight::Height(height)) => unimplemented!(),
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
