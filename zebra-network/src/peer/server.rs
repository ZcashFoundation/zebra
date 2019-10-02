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
        let (mut peer_tx, mut peer_rx) = peer.split();

        // Streams must have items of the same type, so to handle both messages
        // and requests, define a lightweight enum and convert both streams to
        // that type before merging them.
        enum EventType {
            M(Message),
            R(Option<Request>),
            //RS((Request, StreamFuture<Receiver<Request>>)),
        }
        use EventType::*; // Allows using M,R directly.

        'outer: loop {
            match self.req {
                None => {
                    let mut events = stream::select(
                        (&mut peer_rx).map(|m| M(m.unwrap())),
                        stream::once(self.client_rx.next()).map(|r| R(r)),
                    );
                    while let Some(ev) = events.next().await {
                        match ev {
                            M(m) => self.handle_message_as_request(&m).await,
                            R(Some(r)) => {
                                self.handle_client_request(r).await;
                                continue 'outer;
                            }
                            R(None) => {
                                panic!("client channel is over, close the peer");
                            }
                        }
                    }
                }
                Some(_) => {
                    let mut events = (&mut peer_rx).map(|m| m.unwrap());
                    while let Some(m) = events.next().await {
                        if self.handle_message_as_response(&m).await {
                            continue 'outer;
                        } else {
                            self.handle_message_as_request(&m).await;
                        }
                    }
                }
            }
        }
    }

    async fn handle_client_request(&mut self, r: Request) {}
    async fn handle_message_as_request(&mut self, m: &Message) {}
    async fn handle_message_as_response(&mut self, m: &Message) -> bool {
        false
    }
}
