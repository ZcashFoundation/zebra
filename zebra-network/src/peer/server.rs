use std::sync::Arc;

use futures::{
    channel::{mpsc, oneshot},
    future::{self, Either},
    stream::Stream,
};
use tokio::{
    prelude::*,
    timer::{delay_for, Delay},
};
use tower::Service;

use zebra_chain::{serialization::SerializationError, transaction::TransactionHash};

use crate::{
    constants,
    protocol::{
        external::{InventoryHash, Message},
        internal::{Request, Response},
    },
    BoxedStdError,
};

use super::{ClientRequest, ErrorSlot, PeerError, SharedPeerError};

pub(super) enum State {
    /// Awaiting a client request or a peer message.
    AwaitingRequest,
    /// Awaiting a peer message we can interpret as a client request.
    AwaitingResponse(Request, oneshot::Sender<Result<Response, SharedPeerError>>),
    /// A failure has occurred and we are shutting down the server.
    Failed,
}

/// The "server" duplex half of a peer connection.
pub struct Server<S, Tx> {
    pub(super) state: State,
    /// A timeout for a client request. This is stored separately from
    /// State so that we can move the future out of it independently of
    /// other state handling.
    pub(super) request_timer: Option<Delay>,
    pub(super) svc: S,
    pub(super) client_rx: mpsc::Receiver<ClientRequest>,
    /// A slot shared between the client and server for storing an error.
    pub(super) error_slot: ErrorSlot,
    //pub(super) peer_rx: Rx,
    pub(super) peer_tx: Tx,
}

impl<S, Tx> Server<S, Tx>
where
    S: Service<Request, Response = Response, Error = BoxedStdError>,
    S::Error: Into<BoxedStdError>,
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    /// Run this peer server to completion.
    pub async fn run<Rx>(mut self, mut peer_rx: Rx)
    where
        Rx: Stream<Item = Result<Message, SerializationError>> + Unpin,
    {
        // At a high level, the event loop we want is as follows: we check for any
        // incoming messages from the remote peer, check if they should be interpreted
        // as a response to a pending client request, and if not, interpret them as a
        // request from the remote peer to our node.
        //
        // We also need to handle those client requests in the first place. The client
        // requests are received from the corresponding `peer::Client` over a bounded
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
        loop {
            match self.state {
                State::AwaitingRequest => {
                    trace!("awaiting client request or peer message");
                    match future::select(peer_rx.next(), self.client_rx.next()).await {
                        Either::Left((None, _)) => {
                            self.fail_with(PeerError::ConnectionClosed);
                        }
                        // XXX switch back to hard failure when we parse all message types
                        //Either::Left((Some(Err(e)), _)) => self.fail_with(e.into()),
                        Either::Left((Some(Err(e)), _)) => error!(%e),
                        Either::Left((Some(Ok(msg)), _)) => {
                            self.handle_message_as_request(msg).await
                        }
                        Either::Right((None, _)) => {
                            self.fail_with(PeerError::DeadClient);
                        }
                        Either::Right((Some(req), _)) => self.handle_client_request(req).await,
                    }
                }
                // We're awaiting a response to a client request,
                // so wait on either a peer message, or on a request timeout.
                State::AwaitingResponse { .. } => {
                    trace!("awaiting response to client request");
                    let timer_ref = self
                        .request_timer
                        .as_mut()
                        .expect("timeout must be set while awaiting response");
                    match future::select(peer_rx.next(), timer_ref).await {
                        Either::Left((None, _)) => self.fail_with(PeerError::ConnectionClosed),
                        // XXX switch back to hard failure when we parse all message types
                        //Either::Left((Some(Err(e)), _)) => self.fail_with(e.into()),
                        Either::Left((Some(Err(peer_err)), _timer)) => error!(%peer_err),
                        Either::Left((Some(Ok(peer_msg)), _timer)) => {
                            match self.handle_message_as_response(peer_msg) {
                                None => continue,
                                Some(msg) => self.handle_message_as_request(msg).await,
                            }
                        }
                        Either::Right(((), _peer_fut)) => {
                            trace!("client request timed out");
                            // Re-matching lets us take ownership of tx
                            self.state = match self.state {
                                State::AwaitingResponse(Request::Ping(_), _) => {
                                    self.fail_with(PeerError::ClientRequestTimeout);
                                    State::Failed
                                }
                                State::AwaitingResponse(_, tx) => {
                                    let e = PeerError::ClientRequestTimeout;
                                    let _ = tx.send(Err(Arc::new(e).into()));
                                    State::AwaitingRequest
                                }
                                _ => panic!("unreachable"),
                            };
                        }
                    }
                }
                // We've failed, but we need to flush all pending client
                // requests before we can return and complete the future.
                State::Failed => {
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
        debug!(%e, "failing peer service with error");
        // Update the shared error slot
        let mut guard = self
            .error_slot
            .0
            .lock()
            .expect("mutex should be unpoisoned");
        if guard.is_some() {
            panic!("called fail_with on already-failed server state");
        } else {
            *guard = Some(Arc::new(e).into());
        }
        // Drop the guard immediately to release the mutex.
        std::mem::drop(guard);

        // We want to close the client channel and set State::Failed so
        // that we can flush any pending client requests. However, we may have
        // an outstanding client request in State::AwaitingResponse, so
        // we need to deal with it first if it exists.
        self.client_rx.close();
        let old_state = std::mem::replace(&mut self.state, State::Failed);
        if let State::AwaitingResponse(_, tx) = old_state {
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
        use State::*;
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
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse(GetPeers, tx)),
            (AwaitingRequest, PushPeers(addrs)) => self
                .peer_tx
                .send(Message::Addr(addrs))
                .await
                .map_err(|e| e.into())
                .map(|()| {
                    // PushPeers does not have a response, so we return OK as
                    // soon as we send the request. Sending through a oneshot
                    // can only fail if the rx end is dropped before calling
                    // send, which we can safely ignore (the request future was
                    // cancelled).
                    let _ = tx.send(Ok(Response::Ok));
                    AwaitingRequest
                }),
            (AwaitingRequest, Ping(nonce)) => self
                .peer_tx
                .send(Message::Ping(nonce))
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse(Ping(nonce), tx)),
            (AwaitingRequest, GetMempool) => self
                .peer_tx
                .send(Message::Mempool)
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse(GetMempool, tx)),
            // XXX timeout handling here?
        } {
            Ok(new_state) => {
                self.state = new_state;
                self.request_timer = Some(delay_for(constants::REQUEST_TIMEOUT));
            }
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
        use State::*;
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
            // In this special case, we ignore tx, because we handle ping/pong
            // messages internally; the "shadow client" only serves to generate
            // outbound pings for us to process.
            (AwaitingResponse(Ping(req_nonce), _tx), Message::Pong(res_nonce)) => {
                if req_nonce != res_nonce {
                    self.fail_with(PeerError::HeartbeatNonceMismatch);
                }
                AwaitingRequest
            }
            (
                AwaitingResponse(_, tx),
                Message::Reject {
                    message,
                    ccode,
                    reason,
                    data,
                },
            ) => {
                tx.send(Err(SharedPeerError::from(Arc::new(PeerError::Rejected))))
                    .expect("response oneshot should be unused");

                error!(
                    "{:?} message rejected: {:?}, {:?}, {:?}",
                    message, ccode, reason, data
                );
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
                self.fail_with(PeerError::DuplicateHandshake);
                return;
            }
            Message::Verack { .. } => {
                self.fail_with(PeerError::DuplicateHandshake);
                return;
            }
            Message::Ping(nonce) => {
                match self.peer_tx.send(Message::Pong(nonce)).await {
                    Ok(()) => {}
                    Err(e) => self.fail_with(e.into()),
                }
                return;
            }
            _ => {}
        }

        // Interpret `msg` as a request from the remote peer to our node,
        // and try to construct an appropriate request object.
        let req = match msg {
            Message::Addr(addrs) => Some(Request::PushPeers(addrs)),
            Message::GetAddr => Some(Request::GetPeers),
            Message::Mempool => Some(Request::GetMempool),
            _ => {
                debug!("unhandled message type");
                None
            }
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
        use tower::{load_shed::error::Overloaded, ServiceExt};

        if let Err(_) = self.svc.ready().await {
            // Treat all service readiness errors as Overloaded
            self.fail_with(PeerError::Overloaded);
        }

        let rsp = match self.svc.call(req).await {
            Err(e) => {
                if e.is::<Overloaded>() {
                    self.fail_with(PeerError::Overloaded);
                } else {
                    // We could send a reject to the remote peer.
                    error!(%e);
                }
                return;
            }
            Ok(rsp) => rsp,
        };

        match rsp {
            Response::Ok => { /* generic success, do nothing */ }
            Response::Error => {
                if let Err(e) = self.peer_tx.send(Message::from(PeerError::Rejected)).await {
                    self.fail_with(e.into());
                }
            }
            Response::Peers(addrs) => {
                if let Err(e) = self.peer_tx.send(Message::Addr(addrs)).await {
                    self.fail_with(e.into());
                }
            }
            Response::Transactions(txs) => {
                let hashes = txs
                    .into_iter()
                    .map(|tx| InventoryHash::Tx(TransactionHash::from(tx)))
                    .collect::<Vec<_>>();

                if let Err(e) = self.peer_tx.send(Message::Inv(hashes)).await {
                    self.fail_with(e.into());
                }
            }
        }
    }
}
