use std::sync::{Arc, Mutex};

use failure::Error;
use futures::{
    channel::{mpsc, oneshot},
    stream::Stream,
};
use tokio::prelude::*;
use tower::Service;

use crate::protocol::{
    internal::{Request, Response},
    message::Message,
};

use super::{client::ClientRequest, PeerError};

#[derive(Default, Clone)]
pub(super) struct ErrorSlot(Arc<Mutex<Option<PeerError>>>);

impl ErrorSlot {
    pub fn try_get_error(&self) -> Option<PeerError> {
        self.0
            .lock()
            .expect("error mutex should be unpoisoned")
            .as_ref()
            .map(|e| e.clone())
    }
}

pub(super) enum ServerState {
    /// Awaiting a client request or a peer message.
    AwaitingRequest,
    /// Awaiting a peer message we can interpret as a client request.
    AwaitingResponse(Request, oneshot::Sender<Result<Response, PeerError>>),
    /// A failure has occurred and we are shutting down the server.
    Failed,
}

/// The "server" duplex half of a peer connection.
pub struct PeerServer<S, Tx> {
    pub(super) state: ServerState,
    pub(super) svc: S,
    pub(super) client_rx: mpsc::Receiver<ClientRequest>,
    /// A slot shared between the PeerServer and PeerClient for storing an error.
    pub(super) error_slot: ErrorSlot,
    //pub(super) peer_rx: Rx,
    pub(super) peer_tx: Tx,
}

impl<S, Tx> PeerServer<S, Tx>
where
    S: Service<Request, Response = Response>,
    S::Error: Send,
    //S::Error: Into<Error>,
    Tx: Sink<Message> + Unpin,
    Tx::Error: Into<Error>,
{
    /// Run this peer server to completion.
    pub async fn run<Rx>(mut self, mut peer_rx: Rx)
    where
        Rx: Stream<Item = Result<Message, Error>> + Unpin,
    {
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

        use futures::future::FutureExt;
        use futures::select;

        // This future represents the next message received from the peer.
        // It needs to be stored outside of the event loop, so that we can overwrite
        // it with the new "next message future" every time we get a new message.
        let mut peer_rx_fut = peer_rx.next().fuse();
        loop {
            match self.state {
                // We're awaiting a client request, so listen for both
                // client requests and peer messages simultaneously.
                ServerState::AwaitingRequest => select! {
                    req = self.client_rx.next() => {
                        match req {
                            Some(req) => self.handle_client_request(req).await,
                            None => {
                                trace!("client_rx closed, shutting down");
                                return;
                            }
                        }
                    }
                    msg = peer_rx_fut => {
                        peer_rx_fut = peer_rx.next().fuse();
                        match msg {
                            None => {
                                trace!("peer stream closed, shutting down");
                                return;
                            }
                            // We got a peer message but it was malformed.
                            //Some(Err(e)) => self.fail_with(e.into()),
                            // XXX remove this when we parse all message types
                            Some(Err(e)) => {
                                error!(%e);
                            }
                            // We got a peer message and it was well-formed.
                            Some(Ok(msg)) => self.handle_message_as_request(msg).await,
                        }
                    }
                },
                // We're awaiting a response to a client request,
                // so only listen to peer messages, not further requests.
                ServerState::AwaitingResponse { .. } => {
                    let msg = peer_rx_fut.await;
                    peer_rx_fut = peer_rx.next().fuse();
                    match msg {
                        // The peer channel has closed -- no more messages.
                        // However, we still need to flush pending client requests.
                        None => self.fail_with(format_err!("peer closed connection").into()),
                        // We got a peer message but it was malformed.
                        //Some(Err(e)) => self.fail_with(e.into()),
                        // XXX remove this when we parse all message types
                        Some(Err(e)) => {
                            error!(%e);
                        }
                        // We got a peer message and it was well-formed.
                        Some(Ok(msg)) => match self.handle_message_as_response(msg) {
                            None => continue,
                            Some(msg) => self.handle_message_as_request(msg).await,
                        },
                    }
                }
                // We've failed, but we need to flush all pending client
                // requests before we can return and complete the future.
                ServerState::Failed => {
                    match self.client_rx.next().await {
                        Some(ClientRequest(_, tx)) => {
                            let e = self
                                .error_slot
                                .try_get_error()
                                .expect("cannot enter failed state without setting error slot");
                            let _ = tx.send(Err(e));
                            // Continue until we've errored all queued reqs
                            continue;
                        }
                        None => return,
                    }
                }
            }
        }
    }

    /// Marks the peer as having failed with error `e`.
    fn fail_with(&mut self, e: PeerError) {
        trace!(%e, "failing peer service with error");
        // Update the shared error slot
        let mut guard = self
            .error_slot
            .0
            .lock()
            .expect("mutex should be unpoisoned");
        if guard.is_some() {
            panic!("called fail_with on already-failed server state");
        } else {
            *guard = Some(e);
        }
        // Drop the guard immediately to release the mutex.
        std::mem::drop(guard);

        // We want to close the client channel and set ServerState::Failed so
        // that we can flush any pending client requests. However, we may have
        // an outstanding client request in ServerState::AwaitingResponse, so
        // we need to deal with it first if it exists.
        self.client_rx.close();
        let old_state = std::mem::replace(&mut self.state, ServerState::Failed);
        if let ServerState::AwaitingResponse(_, tx) = old_state {
            // We know the slot has Some(e) because we just set it above,
            // and the error slot is never unset.
            let e = self.error_slot.try_get_error().unwrap();
            let _ = tx.send(Err(e));
        }
    }

    /// Handle an incoming client request, possibly generating outgoing messages to the
    /// remote peer.
    async fn handle_client_request(&mut self, msg: ClientRequest) {
        trace!(?msg);
        use Request::*;
        use ServerState::*;
        let ClientRequest(req, tx) = msg;

        // Inner match returns Result with the new state or an error.
        // Outer match updates state or fails.
        match match (&self.state, req) {
            (Failed, _) => panic!("failed service cannot handle requests"),
            (AwaitingResponse { .. }, _) => panic!("tried to update pending request"),
            (AwaitingRequest, GetPeers) => self
                .peer_tx
                .send(Message::GetAddr)
                .await
                .map_err(|e| e.into().into())
                .map(|()| AwaitingResponse(GetPeers, tx)),
            (AwaitingRequest, PushPeers(addrs)) => self
                .peer_tx
                .send(Message::Addr(addrs))
                .await
                .map_err(|e| e.into().into())
                .map(|()| {
                    // PushPeers does not have a response, so we return OK as
                    // soon as we send the request. Sending through a oneshot
                    // can only fail if the rx end is dropped before calling
                    // send, which we can safely ignore (the request future was
                    // cancelled).
                    let _ = tx.send(Ok(Response::Ok));
                    AwaitingRequest
                }),
        } {
            Ok(new_state) => self.state = new_state,
            Err(e) => self.fail_with(e),
        }
    }

    /// Try to handle `msg` as a response to a client request, possibly consuming
    /// it in the process.
    ///
    /// Taking ownership of the message means that we can pass ownership of its
    /// contents to responses without additional copies.  If the message is not
    /// interpretable as a response, we return ownership to the caller.
    fn handle_message_as_response(&mut self, msg: Message) -> Option<Message> {
        trace!(?msg);
        // This function is where we statefully interpret Bitcoin/Zcash messages
        // into responses to messages in the internal request/response protocol.
        // This conversion is done by a sequence of (request, message) match arms,
        // each of which contains the conversion logic for that pair.
        use Request::*;
        use ServerState::*;
        let mut ignored_msg = None;
        // We want to be able to consume the state, but it's behind a mutable
        // reference, so we can't move it out of self without swapping in a
        // placeholder, even if we immediately overwrite the placeholder.
        let tmp_state = std::mem::replace(&mut self.state, AwaitingRequest);
        self.state = match (tmp_state, msg) {
            (AwaitingResponse(GetPeers, tx), Message::Addr(addrs)) => {
                tx.send(Ok(Response::Peers(addrs)))
                    .expect("response oneshot should be unused");
                AwaitingRequest
            }
            // By default, messages are not responses.
            (state, msg) => {
                ignored_msg = Some(msg);
                state
            }
        };

        ignored_msg
    }

    async fn handle_message_as_request(&mut self, msg: Message) {
        trace!(?msg);
        // These messages are transport-related, handle them separately:
        match msg {
            Message::Version { .. } => {
                self.fail_with(format_err!("got version message after handshake").into());
                return;
            }
            Message::Verack { .. } => {
                self.fail_with(format_err!("got verack message after handshake").into());
                return;
            }
            Message::Ping(nonce) => {
                match self.peer_tx.send(Message::Pong(nonce)).await {
                    Ok(()) => {}
                    Err(e) => self.fail_with(e.into().into()),
                }
                return;
            }
            _ => {}
        }

        // Interpret `msg` as a request from the remote peer to our node,
        // and try to construct an appropriate request object.
        let req = match msg {
            Message::Addr(addrs) => Some(Request::PushPeers(addrs)),
            _ => None,
        };

        match req {
            Some(req) => self.drive_peer_request(req).await,
            None => {}
        }
    }

    /// Given a `req` originating from the peer, drive it to completion and send
    /// any appropriate messages to the remote peer. If an error occurs while
    /// processing the request (e.g., the service is shedding load), then we call
    /// fail_with to terminate the entire peer connection, shrinking the number
    /// of connected peers.
    async fn drive_peer_request(&mut self, req: Request) {
        trace!(?req);
        use tower::ServiceExt;
        // XXX Drop the errors on the floor for now so that
        // we can ignore error type alignment
        match self.svc.ready().await {
            Err(_) => self.fail_with(format_err!("svc err").into()),
            Ok(()) => {}
        }
        match self.svc.call(req).await {
            Err(_) => self.fail_with(format_err!("svc err").into()),
            Ok(Response::Ok) => { /* generic success, do nothing */ }
            Ok(Response::Peers(addrs)) => {
                if let Err(e) = self.peer_tx.send(Message::Addr(addrs)).await {
                    self.fail_with(e.into().into());
                }
            }
        }
    }
}
