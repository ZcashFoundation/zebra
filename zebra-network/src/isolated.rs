//! Code for creating isolated connections to specific peers.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::{FutureExt, TryFutureExt};

use tokio::net::TcpStream;
use tower::{
    util::{BoxService, Oneshot},
    Service,
};

use crate::{peer, BoxedStdError, Config, Request, Response};

/// Use the provided TCP connection to create a Zcash connection completely
/// isolated from all other node state.
///
/// The connection pool returned by `init` should be used for all requests that
/// don't require isolated state or use of an existing TCP connection. However,
/// this low-level API is useful for custom network crawlers or Tor connections.
///
/// In addition to being completely isolated from all other node state, this
/// method also aims to be minimally distinguishable from other clients.
///
/// Note that this method does not implement any timeout behavior, so callers may
/// want to layer it with a timeout as appropriate for their application.
///
/// # Inputs
///
/// - `conn`: an existing TCP connection to use. Passing an existing TCP
/// connection allows this method to be used with clearnet or Tor transports.
///
/// - `user_agent`: a valid BIP14 user-agent, e.g., the empty string.
pub async fn connect_isolated(
    conn: TcpStream,
    user_agent: String,
) -> Result<BoxService<Request, Response, BoxedStdError>, BoxedStdError> {
    let handshake = peer::Handshake::builder()
        .with_config(Config::default())
        .with_inbound_service(tower::service_fn(|_req| async move {
            Ok::<Response, BoxedStdError>(Response::Nil)
        }))
        .with_user_agent(user_agent)
        .finish()
        .expect("provided mandatory builder parameters");

    // We can't get the remote addr from conn, because it might be a tcp
    // connection through a socks proxy, not directly to the remote. But it
    // doesn't seem like zcashd cares if we give a bogus one, and Zebra doesn't
    // touch it at all.
    let remote_addr = "0.0.0.0:8233".parse().unwrap();

    let client = Oneshot::new(handshake, (conn, remote_addr)).await?;

    Ok(BoxService::new(Wrapper(client)))
}

// This can be deleted when a new version of Tower with map_err is released.
struct Wrapper(peer::Client);

impl Service<Request> for Wrapper {
    type Response = Response;
    type Error = BoxedStdError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.0.call(req).map_err(Into::into).boxed()
    }
}
