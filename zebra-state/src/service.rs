use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, util::BoxService, Service};
use zebra_chain::parameters::Network;

use crate::{BoxError, Config, MemoryState, Request, Response, SledState};

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
            Request::AddBlock { block } => unimplemented!(),
            Request::GetBlock { hash } => unimplemented!(),
            Request::GetTip => unimplemented!(),
            Request::GetDepth { hash } => unimplemented!(),
            Request::GetBlockLocator { genesis } => unimplemented!(),
        }
    }
}

/// Initialize a state service from the provided `config`.
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
