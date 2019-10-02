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
        // At a high level, the event loop we want is as follows: we check for
        // any incoming messages from the remote peer, check if they should be
        // interpreted as a response to a pending client request, and if not,
        // interpret them as a request from the remote peer to our node.
        //
        // We also need to handle those client requests in the first place. The
        // client requests are received from the corresponding `PeerClient` over
        // a bounded channel (with bound 1, to minimize buffering), but there is
        // no relationship between the stream of client requests and the stream
        // of peer messages, so we cannot ignore one kind while waiting on the
        // other. Moreover, we cannot accept a second client request while the
        // first one is still pending.
        //
        // Handling two async streams simultaneously can be done using
        // stream::select, which interleaves the two input streams in order of
        // their items' readiness. However, we cannot just interleave the stream
        // of incoming peer messages and the stream of incoming client requests,
        // because we cannot accept a second client request while the first is
        // still pending.
        //
        // Instead, we inspect the current request state.
        //
        // If there is a pending request, we process only the stream of incoming
        // peer messages until we see one that could be interpreted as a
        // response, and then erase the pending request.
        //
        // If there is no pending request, we construct a stream containing only
        // the next client request and use `stream::select` to join it with the
        // stream of peer messages, then process the combined stream until we
        // see a request.

        let (mut peer_tx, mut peer_rx) = peer.split();

        // Streams must have items of the same type, so to handle both messages
        // and requests, define a lightweight enum and convert both streams to
        // that type before merging them.
        enum EventType {
            M(Message),
            R(Request),
        }
        use EventType::*; // Allows using M,R directly.

        'outer: loop {
            let events = match self.req {
                None => stream::select(
                    (&mut peer_rx).map(|m| M(m.unwrap())),
                    stream::once(self.client_rx.next()).map(|r| R(r.unwrap())),
                ),
                // this doesn't actually work because match arms have incompatible types
                Some(_) => (&mut peer_rx).map(|m| M(m.unwrap())),
            };
            while let Some(ev) = events.next().await {
                match ev {
                    M(m) => {
                        if self.handle_message_as_response(&m).await {
                            self.req = None;
                            continue 'outer;
                        } else {
                            self.handle_message_as_request(&m).await;
                        }
                    }
                    R(r) => {
                        self.handle_client_request(r).await;
                        self.req = Some(r);
                        continue 'outer;
                    }
                }
            }
        }
    }

    async fn handle_client_request(&mut self, r: Request) {
        if self.req.is_some() {
            panic!("tried to overwrite a pending client request");
        }
        // do other processing, e.g., send messages
        unimplemented!();
    }

    async fn handle_message_as_request(&mut self, m: &Message) {
        // construct internal Req and send to svc
        unimplemented!();
    }

    async fn handle_message_as_response(&mut self, m: &Message) -> bool {
        // if we have no pending request, it cannot be a response
        if self.req.is_none() {
            return false;
        }

        // check if message is a response to request
        match self.req {
            Some(_) => {
                // for each Some(Request::Kind), check if the message is a resp
                // and if so, process it, send a resp through the channel
                false
            }
        }
    }
}
