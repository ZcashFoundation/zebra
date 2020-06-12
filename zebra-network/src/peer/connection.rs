use std::collections::HashSet;
use std::sync::Arc;

use futures::{
    channel::{mpsc, oneshot},
    future::{self, Either},
    prelude::*,
    stream::Stream,
};
use tokio::time::{delay_for, Delay};
use tower::Service;

use zebra_chain::{
    block::{Block, BlockHeaderHash},
    serialization::SerializationError,
};

use crate::{
    constants,
    protocol::{
        external::{types::Nonce, InventoryHash, Message},
        internal::{Request, Response},
    },
    BoxedStdError,
};

use super::{ClientRequest, ErrorSlot, PeerError, SharedPeerError};

pub(super) enum Handler {
    /// Indicates that the handler has finished processing the request.
    Finished(Result<Response, SharedPeerError>),
    Ping(Nonce),
    GetPeers,
    GetBlocksByHash {
        hashes: HashSet<BlockHeaderHash>,
        blocks: Vec<Arc<Block>>,
    },
    FindBlocks,
}

impl Handler {
    /// Try to handle `msg` as a response to a client request, possibly consuming
    /// it in the process.
    ///
    /// Taking ownership of the message means that we can pass ownership of its
    /// contents to responses without additional copies.  If the message is not
    /// interpretable as a response, we return ownership to the caller.
    fn process_message(&mut self, msg: Message) -> Option<Message> {
        trace!(?msg);
        // This function is where we statefully interpret Bitcoin/Zcash messages
        // into responses to messages in the internal request/response protocol.
        // This conversion is done by a sequence of (request, message) match arms,
        // each of which contains the conversion logic for that pair.
        use Handler::*;
        let mut ignored_msg = None;
        // XXX can this be avoided?
        let tmp_state = std::mem::replace(self, Finished(Ok(Response::Nil)));
        *self = match (tmp_state, msg) {
            (Ping(req_nonce), Message::Pong(rsp_nonce)) => {
                if req_nonce == rsp_nonce {
                    Finished(Ok(Response::Nil))
                } else {
                    Ping(req_nonce)
                }
            }
            (GetPeers, Message::Addr(addrs)) => Finished(Ok(Response::Peers(addrs))),
            (
                GetBlocksByHash {
                    mut hashes,
                    mut blocks,
                },
                Message::Block(block),
            ) => {
                if hashes.remove(&BlockHeaderHash::from(block.as_ref())) {
                    blocks.push(block);
                    if hashes.is_empty() {
                        Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        GetBlocksByHash { hashes, blocks }
                    }
                } else {
                    Finished(Err(PeerError::WrongBlock.into()))
                }
            }
            (FindBlocks, Message::Inv(inv_hashes)) => Finished(Ok(Response::BlockHeaderHashes(
                inv_hashes
                    .into_iter()
                    .filter_map(|inv| match inv {
                        InventoryHash::Block(hash) => Some(hash),
                        _ => None,
                    })
                    .collect(),
            ))),
            // By default, messages are not responses.
            (state, msg) => {
                ignored_msg = Some(msg);
                state
            }
        };

        ignored_msg
    }
}

pub(super) enum State {
    /// Awaiting a client request or a peer message.
    AwaitingRequest,
    /// Awaiting a peer message we can interpret as a client request.
    AwaitingResponse(Handler, oneshot::Sender<Result<Response, SharedPeerError>>),
    /// A failure has occurred and we are shutting down the connection.
    Failed,
}

/// The state associated with a peer connection.
pub struct Connection<S, Tx> {
    pub(super) state: State,
    /// A timeout for a client request. This is stored separately from
    /// State so that we can move the future out of it independently of
    /// other state handling.
    pub(super) request_timer: Option<Delay>,
    pub(super) svc: S,
    pub(super) client_rx: mpsc::Receiver<ClientRequest>,
    /// A slot for an error shared between the Connection and the Client that uses it.
    pub(super) error_slot: ErrorSlot,
    //pub(super) peer_rx: Rx,
    pub(super) peer_tx: Tx,
}

impl<S, Tx> Connection<S, Tx>
where
    S: Service<Request, Response = Response, Error = BoxedStdError>,
    S::Error: Into<BoxedStdError>,
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    /// Consume this `Connection` to form a spawnable future containing its event loop.
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
                        Either::Left((Some(Err(e)), _)) => self.fail_with(e.into()),
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
                        Either::Left((Some(Err(e)), _)) => self.fail_with(e.into()),
                        Either::Left((Some(Ok(peer_msg)), _timer)) => {
                            // Try to process the message using the handler.
                            // This extremely awkward construction avoids
                            // keeping a live reference to handler across the
                            // call to handle_message_as_request, which takes
                            // &mut self. This is a sign that we don't properly
                            // factor the state required for inbound and
                            // outbound requests.
                            let request_msg = match self.state {
                                State::AwaitingResponse(ref mut handler, _) => {
                                    handler.process_message(peer_msg)
                                }
                                _ => unreachable!(),
                            };
                            // If the message was not consumed, check whether it
                            // should be handled as a request.
                            if let Some(msg) = request_msg {
                                self.handle_message_as_request(msg).await;
                            } else {
                                // Otherwise, check whether the handler is finished
                                // processing messages and update the state.
                                self.state = match self.state {
                                    State::AwaitingResponse(Handler::Finished(response), tx) => {
                                        let _ = tx.send(response);
                                        State::AwaitingRequest
                                    }
                                    pending @ State::AwaitingResponse(_, _) => pending,
                                    _ => unreachable!(),
                                };
                            }
                        }
                        Either::Right(((), _peer_fut)) => {
                            trace!("client request timed out");
                            let e = PeerError::ClientRequestTimeout;
                            self.state = match self.state {
                                // Special case: ping timeouts fail the connection.
                                State::AwaitingResponse(Handler::Ping(_), _) => {
                                    self.fail_with(e);
                                    State::Failed
                                }
                                // Other request timeouts fail the request.
                                State::AwaitingResponse(_, tx) => {
                                    let _ = tx.send(Err(e.into()));
                                    State::AwaitingRequest
                                }
                                _ => unreachable!(),
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
            panic!("called fail_with on already-failed connection state");
        } else {
            *guard = Some(e.into());
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
            (Failed, _) => panic!("failed connection cannot handle requests"),
            (AwaitingResponse { .. }, _) => panic!("tried to update pending request"),
            (AwaitingRequest, Peers) => self
                .peer_tx
                .send(Message::GetAddr)
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse(Handler::GetPeers, tx)),
            (AwaitingRequest, Ping(nonce)) => self
                .peer_tx
                .send(Message::Ping(nonce))
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse(Handler::Ping(nonce), tx)),
            (AwaitingRequest, BlocksByHash(hashes)) => self
                .peer_tx
                .send(Message::GetData(
                    hashes.iter().map(|h| (*h).into()).collect(),
                ))
                .await
                .map_err(|e| e.into())
                .map(|()| {
                    AwaitingResponse(
                        Handler::GetBlocksByHash {
                            blocks: Vec::with_capacity(hashes.len()),
                            hashes,
                        },
                        tx,
                    )
                }),
            (AwaitingRequest, FindBlocks { known_blocks, stop }) => self
                .peer_tx
                .send(Message::GetBlocks {
                    block_locator_hashes: known_blocks,
                    hash_stop: stop.unwrap_or(BlockHeaderHash([0; 32])),
                })
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse(Handler::FindBlocks, tx)),
        } {
            Ok(new_state) => {
                self.state = new_state;
                self.request_timer = Some(delay_for(constants::REQUEST_TIMEOUT));
            }
            Err(e) => self.fail_with(e),
        }
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

        // Per BIP-011, since we don't advertise NODE_BLOOM, we MUST
        // disconnect from this peer immediately.
        match msg {
            Message::FilterLoad { .. }
            | Message::FilterAdd { .. }
            | Message::FilterClear { .. } => {
                self.fail_with(PeerError::UnsupportedMessage);
                return;
            }
            _ => {}
        }

        // Interpret `msg` as a request from the remote peer to our node,
        // and try to construct an appropriate request object.
        let req = match msg {
            Message::Addr(_) => {
                debug!("ignoring unsolicited addr message");
                None
            }
            Message::GetAddr => Some(Request::Peers),
            _ => {
                debug!("unhandled message type");
                None
            }
        };

        if let Some(req) = req {
            self.drive_peer_request(req).await
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

        if self.svc.ready_and().await.is_err() {
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
            Response::Nil => { /* generic success, do nothing */ }
            Response::Peers(addrs) => {
                if let Err(e) = self.peer_tx.send(Message::Addr(addrs)).await {
                    self.fail_with(e.into());
                }
            }
            Response::Blocks(blocks) => {
                // Generate one block message per block.
                for block in blocks.into_iter() {
                    if let Err(e) = self.peer_tx.send(Message::Block(block)).await {
                        self.fail_with(e.into());
                    }
                }
            }
            Response::BlockHeaderHashes(hashes) => {
                if let Err(e) = self
                    .peer_tx
                    .send(Message::Inv(hashes.into_iter().map(Into::into).collect()))
                    .await
                {
                    self.fail_with(e.into())
                }
            }
        }
    }
}
