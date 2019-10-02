use std::net::SocketAddr;

use failure::Error;
use futures::{
    channel::mpsc,
    stream::{self, Stream, StreamExt},
};
use tokio::{codec::Framed, net::TcpStream, prelude::*};
use tower::Service;

use crate::protocol::{
    codec::Codec,
    internal::{Request, Response},
    message::Message,
};

/// The "server" duplex half of a peer connection.
pub struct PeerServer<S> {
    pub(super) addr: SocketAddr,
    pub(super) req: Option<Request>,
    //pub(super) peer: Framed<TcpStream, Codec>,
    pub(super) client_rx: mpsc::Receiver<Request>,
    pub(super) client_tx: mpsc::Sender<Response>,
    pub(super) svc: S,
}

impl<S> PeerServer<S>
where
    S: Service<Request>,
    S::Response: Into<Response>,
    S::Error: Into<Error>,
{
    async fn run(mut self, mut peer: Framed<TcpStream, Codec>) {
        // At a high level, the event loop we want is as follows: we check for any
        // incoming messages from the remote peer, check if they should be interpreted
        // as a response to a pending client request, and if not, interpret them as a
        // request from the remote peer to our node.
        //
        // We also need to handle those client requests in the first place. The client
        // requests are received from the corresponding `PeerClient` over a bounded
        // channel (with bound 1, to minimize buffering), but there is no relationship
        // between the stream of client requests and the stream of peer messages, so we
        // cannot ignore one kind while waiting on the other. Moreover, we cannot accept
        // a second client request while the first one is still pending.
        //
        // To do this, we inspect the current request state.
        //
        // If there is no pending request, we wait on either an incoming peer message or
        // an incoming request, whichever comes first.
        //
        // If there is a pending request, we wait only on an incoming peer message, and
        // check whether it can be interpreted as a response to the pending request.
        let (mut peer_tx, mut peer_rx) = peer.split();

        use futures::future::FutureExt;
        use futures::select;

        let mut peer_rx_fut = peer_rx.next().fuse();
        loop {
            match self.req {
                None => select! {
                    req = self.client_rx.next() => {
                        info!(req = ?req.as_ref().unwrap());
                        self.handle_client_request(req.as_ref().unwrap()).await;
                        self.req = req;
                    }
                    msg = peer_rx_fut => {
                        peer_rx_fut = peer_rx.next().fuse();
                        let msg = msg.unwrap().unwrap();
                        info!(msg = ?msg);
                        self.handle_message_as_request(&msg).await;
                    }
                },
                Some(_) => {
                    let msg = peer_rx_fut.await.unwrap().unwrap();
                    peer_rx_fut = peer_rx.next().fuse();
                    if self.handle_message_as_response(&msg).await {
                        self.req = None;
                    } else {
                        self.handle_message_as_request(&msg).await;
                    }
                }
            }
        }
    }

    async fn handle_client_request(&mut self, r: &Request) {
        // do other processing, e.g., send messages
        unimplemented!();
    }

    async fn handle_message_as_request(&mut self, m: &Message) {
        // construct internal Req and send to svc
        unimplemented!();
    }

    async fn handle_message_as_response(&mut self, m: &Message) -> bool {
        // check if message is a response to request
        match self.req {
            // if we have no pending request, it cannot be a response
            None => false,
            Some(_) => {
                // for each Some(Request::Kind), check if the message is a resp
                // and if so, process it, send a resp through the channel
                false
            }
        }
    }
}
