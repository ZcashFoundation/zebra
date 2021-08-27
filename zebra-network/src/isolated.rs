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

use zebra_chain::chain_tip::NoChainTip;

use crate::{
    peer::{self, ConnectedAddr},
    BoxError, Config, Request, Response,
};

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
///
/// # Bug
///
/// `connect_isolated` only works on `Mainnet`, see #1687.
pub fn connect_isolated(
    conn: TcpStream,
    user_agent: String,
) -> impl Future<
    Output = Result<
        BoxService<Request, Response, Box<dyn std::error::Error + Send + Sync + 'static>>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    >,
> {
    let handshake = peer::Handshake::builder()
        .with_config(Config::default())
        .with_inbound_service(tower::service_fn(|_req| async move {
            Ok::<Response, Box<dyn std::error::Error + Send + Sync + 'static>>(Response::Nil)
        }))
        .with_user_agent(user_agent)
        .with_chain_tip_receiver(NoChainTip)
        .finish()
        .expect("provided mandatory builder parameters");

    // Don't send any metadata about the connection
    let connected_addr = ConnectedAddr::new_isolated();

    Oneshot::new(handshake, (conn, connected_addr))
        .map_ok(|client| BoxService::new(Wrapper(client)))
}

// This can be deleted when a new version of Tower with map_err is released.
struct Wrapper(peer::Client);

impl Service<Request> for Wrapper {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        self.0.call(req).map_err(Into::into).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_isolated_sends_minimally_distinguished_version_message() {
        use crate::{
            protocol::external::{Codec, Message},
            types::PeerServices,
        };
        use futures::stream::StreamExt;
        use tokio_util::codec::Framed;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen_addr = listener.local_addr().unwrap();

        let conn = tokio::net::TcpStream::connect(listen_addr).await.unwrap();

        tokio::spawn(connect_isolated(conn, "".to_string()));

        let (conn, _) = listener.accept().await.unwrap();

        let mut stream = Framed::new(conn, Codec::builder().finish());
        if let Message::Version {
            services,
            timestamp,
            address_from,
            user_agent,
            start_height,
            relay,
            ..
        } = stream
            .next()
            .await
            .expect("stream item")
            .expect("item is Ok(msg)")
        {
            // Check that the version message sent by connect_isolated
            // has the fields specified in the Stolon RFC.
            assert_eq!(services, PeerServices::empty());
            assert_eq!(timestamp.timestamp() % (5 * 60), 0);
            assert_eq!(
                address_from,
                (PeerServices::empty(), "0.0.0.0:8233".parse().unwrap())
            );
            assert_eq!(user_agent, "");
            assert_eq!(start_height.0, 0);
            assert!(!relay);
        } else {
            panic!("handshake did not send version message");
        }
    }
}
