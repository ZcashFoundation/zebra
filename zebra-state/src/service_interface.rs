use std::fmt;
use std::sync::Arc;
use std::task::Context;
use std::{future::Future, pin::Pin, task::Poll};

use futures::{future::TryFutureExt, FutureExt};

use tower::util::ReadyOneshot;
use zebra_chain::block::{self, Block};

use crate::{Request, Response};

pub trait ZebraStateRead: tower::Service<Request, Response = Response>
where
    Self::Future: Send + 'static,
{
    fn tip(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<(block::Height, block::Hash)>, Self::Error>>>>
    {
        self.call(Request::Tip)
            .map_ok(|response| match response {
                Response::Tip(height_hash) => height_hash,
                _ => unreachable!("Request::Tip always responds with Response::Tip"),
            })
            .boxed()
    }
}

///
pub trait ZebraStateWrite: tower::Service<Request, Response = Response>
where
    Self::Future: Send + 'static,
{
    fn commit_block(
        &mut self,
        block: Arc<Block>,
    ) -> Pin<Box<dyn Future<Output = Result<block::Hash, Self::Error>> + Send + 'static>> {
        self.call(Request::CommitBlock { block })
            .map_ok(|response| match response {
                Response::Committed(hash) => hash,
                _ => unreachable!("Request::CommitBlock always responds with Response::Committed"),
            })
            .boxed()
    }
}

#[must_use]
pub struct ReadyService<'a, S>(&'a mut S);

impl<S> ZebraStateRead for ReadyService<'_, S>
where
    S: tower::Service<Request, Response = Response>,
    S::Future: Send + 'static,
{
}

impl<S> ZebraStateWrite for ReadyService<'_, S>
where
    S: tower::Service<Request, Response = Response>,
    S::Future: Send + 'static,
{
}

impl<S> tower::Service<Request> for ReadyService<'_, S>
where
    S: tower::Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.0.call(req)
    }
}

///
pub trait ZebraStateReady: tower::Service<Request> + Sized {
    fn ready_and2(&mut self) -> AFineReadyAnd<'_, Self, Request> {
        AFineReadyAnd::new(self)
    }
}

impl<S> ZebraStateReady for S where S: tower::Service<Request> {}

/// A future that yields a mutable reference to the service wrapped in a new type
/// that enables our `call` equivalent extension traits
///
/// `ReadyAnd` values are produced by `ServiceExt::ready_and`.
pub struct AFineReadyAnd<'a, T, Request>(ReadyOneshot<&'a mut T, Request>);

// Safety: This is safe for the same reason that the impl for ReadyOneshot is safe.
impl<'a, T, Request> Unpin for AFineReadyAnd<'a, T, Request> {}

impl<'a, T, Request> AFineReadyAnd<'a, T, Request>
where
    T: tower::Service<Request>,
{
    #[allow(missing_docs)]
    pub fn new(service: &'a mut T) -> Self {
        Self(ReadyOneshot::new(service))
    }
}

impl<'a, T, Request> Future for AFineReadyAnd<'a, T, Request>
where
    T: tower::Service<Request>,
{
    type Output = Result<ReadyService<'a, T>, T::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Ready(Ok(ready)) => Poll::Ready(Ok(ReadyService(ready))),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<'a, T, Request> fmt::Debug for AFineReadyAnd<'a, T, Request>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AFineReadyAnd").field(&self.0).finish()
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::{block, parameters::Network, serialization::ZcashDeserializeInto};

    #[tokio::test]
    async fn basic_read_usage() {
        use super::ZebraStateRead;
        use super::ZebraStateReady;

        let config = crate::Config::ephemeral();
        let network = Network::Mainnet;
        let mut service = crate::init(config, network);

        // works
        let _height_hash: Option<(block::Height, block::Hash)> =
            service.ready_and2().await.unwrap().tip().await.unwrap();

        // Does not compile, `ZebraStateReady` isn't implemented directly on `service`
        // let (height, hash) = service
        //     .tip()
        //     .await
        //     .unwrap()
        //     .unwrap();

        // Doesn't compile, ZebraStateWrite isn't in scope
        // let (height, hash) = service
        //     .ready_and2()
        //     .await
        //     .unwrap()
        //     .commit_block()
        //     .await
        //     .unwrap()
        //     .unwrap();
    }

    #[tokio::test]
    async fn basic_write_usage() {
        use super::ZebraStateReady;
        use super::ZebraStateWrite;

        let config = crate::Config::ephemeral();
        let network = Network::Mainnet;
        let mut service = crate::init(config, network);

        let block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into()
            .unwrap();

        // works
        let _hash: block::Hash = service
            .ready_and2()
            .await
            .unwrap()
            .commit_block(block)
            .await
            .unwrap();
    }
}
