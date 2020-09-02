// NOT A PLACE OF HONOR
//
// NO ESTEEMED DEED IS COMMEMORATED HERE
//
// NOTHING VALUED IS HERE
//
// What is here was dangerous and repulsive to us. This message is a warning
// about danger.
//
// The danger is in a particular module... it increases towards a center... the
// center of danger is pub async fn Connection::run the danger is still present,
// in your time, as it was in ours.
//
// The danger is to the mind. The danger is unleashed only if you substantially
// disturb this code. This code is best shunned and left encapsulated.

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
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, Block},
    serialization::SerializationError,
    transaction::{self, Transaction},
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
    Peers,
    FindBlocks,
    BlocksByHash {
        hashes: HashSet<block::Hash>,
        blocks: Vec<Arc<Block>>,
    },
    TransactionsByHash {
        hashes: HashSet<transaction::Hash>,
        transactions: Vec<Arc<Transaction>>,
    },
}

impl Handler {
    /// Try to handle `msg` as a response to a client request, possibly consuming
    /// it in the process.
    ///
    /// Taking ownership of the message means that we can pass ownership of its
    /// contents to responses without additional copies.  If the message is not
    /// interpretable as a response, we return ownership to the caller.
    fn process_message(&mut self, msg: Message) -> Option<Message> {
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
            (Peers, Message::Addr(addrs)) => Finished(Ok(Response::Peers(addrs))),
            (
                TransactionsByHash {
                    mut hashes,
                    mut transactions,
                },
                Message::Tx(transaction),
            ) => {
                if hashes.remove(&transaction.hash()) {
                    transactions.push(transaction);
                    if hashes.is_empty() {
                        Finished(Ok(Response::Transactions(transactions)))
                    } else {
                        TransactionsByHash {
                            hashes,
                            transactions,
                        }
                    }
                } else {
                    // This transaction isn't the one we asked for,
                    // but unsolicited transactions are OK, so leave
                    // for future handling.
                    ignored_msg = Some(Message::Tx(transaction));
                    TransactionsByHash {
                        hashes,
                        transactions,
                    }
                }
            }
            (
                BlocksByHash {
                    mut hashes,
                    mut blocks,
                },
                Message::Block(block),
            ) => {
                if hashes.remove(&block.hash()) {
                    blocks.push(block);
                    if hashes.is_empty() {
                        Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        BlocksByHash { hashes, blocks }
                    }
                } else {
                    // Blocks shouldn't be sent unsolicited,
                    // so fail the request if we got the wrong one.
                    Finished(Err(PeerError::WrongBlock.into()))
                }
            }
            (FindBlocks, Message::Inv(inv_hashes)) => Finished(Ok(Response::BlockHashes(
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
                trace!(?msg, "did not interpret message as response");
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
    AwaitingResponse {
        handler: Handler,
        tx: oneshot::Sender<Result<Response, SharedPeerError>>,
        span: tracing::Span,
    },
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
                            trace!("client_rx closed, ending connection");
                            return;
                        }
                        Either::Right((Some(req), _)) => {
                            let span = req.span.clone();
                            self.handle_client_request(req).instrument(span).await
                        }
                    }
                }
                // We're awaiting a response to a client request,
                // so wait on either a peer message, or on a request timeout.
                State::AwaitingResponse { ref span, .. } => {
                    // we have to get rid of the span reference so we can tamper with the state
                    let span = span.clone();
                    trace!(parent: &span, "awaiting response to client request");
                    let timer_ref = self
                        .request_timer
                        .as_mut()
                        .expect("timeout must be set while awaiting response");
                    match future::select(peer_rx.next(), timer_ref)
                        .instrument(span.clone())
                        .await
                    {
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
                                State::AwaitingResponse {
                                    ref mut handler, ..
                                } => span.in_scope(|| handler.process_message(peer_msg)),
                                _ => unreachable!(),
                            };
                            // If the message was not consumed, check whether it
                            // should be handled as a request.
                            if let Some(msg) = request_msg {
                                // do NOT instrument with the request span, this is
                                // independent work
                                self.handle_message_as_request(msg).await;
                            } else {
                                // Otherwise, check whether the handler is finished
                                // processing messages and update the state.
                                self.state = match self.state {
                                    State::AwaitingResponse {
                                        handler: Handler::Finished(response),
                                        tx,
                                        ..
                                    } => {
                                        let _ = tx.send(response);
                                        State::AwaitingRequest
                                    }
                                    pending @ State::AwaitingResponse { .. } => pending,
                                    _ => unreachable!(),
                                };
                            }
                        }
                        Either::Right(((), _peer_fut)) => {
                            trace!(parent: &span, "client request timed out");
                            let e = PeerError::ClientRequestTimeout;
                            self.state = match self.state {
                                // Special case: ping timeouts fail the connection.
                                State::AwaitingResponse {
                                    handler: Handler::Ping(_),
                                    ..
                                } => {
                                    self.fail_with(e);
                                    State::Failed
                                }
                                // Other request timeouts fail the request.
                                State::AwaitingResponse { tx, .. } => {
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
                        Some(ClientRequest { tx, span, .. }) => {
                            trace!(
                                parent: &span,
                                "erroring pending request to failed connection"
                            );
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
        if let State::AwaitingResponse { tx, .. } = old_state {
            // We know the slot has Some(e) because we just set it above,
            // and the error slot is never unset.
            let e = self.error_slot.try_get_error().unwrap();
            let _ = tx.send(Err(e));
        }
    }

    /// Handle an incoming client request, possibly generating outgoing messages to the
    /// remote peer.
    ///
    /// NOTE: the caller should use .instrument(msg.span) to instrument the function.
    async fn handle_client_request(&mut self, req: ClientRequest) {
        trace!(?req.request);
        use Request::*;
        use State::*;
        let ClientRequest { request, tx, span } = req;

        // XXX(hdevalence) this is truly horrible, but let's fix it later

        // Inner match returns Result with the new state or an error.
        // Outer match updates state or fails.
        match match (&self.state, request) {
            (Failed, _) => panic!("failed connection cannot handle requests"),
            (AwaitingResponse { .. }, _) => panic!("tried to update pending request"),
            (AwaitingRequest, Peers) => self
                .peer_tx
                .send(Message::GetAddr)
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::Peers,
                    tx,
                    span,
                }),
            (AwaitingRequest, Ping(nonce)) => self
                .peer_tx
                .send(Message::Ping(nonce))
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::Ping(nonce),
                    tx,
                    span,
                }),
            (AwaitingRequest, BlocksByHash(hashes)) => self
                .peer_tx
                .send(Message::GetData(
                    hashes.iter().map(|h| (*h).into()).collect(),
                ))
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::BlocksByHash {
                        blocks: Vec::with_capacity(hashes.len()),
                        hashes,
                    },
                    tx,
                    span,
                }),
            (AwaitingRequest, TransactionsByHash(hashes)) => self
                .peer_tx
                .send(Message::GetData(
                    hashes.iter().map(|h| (*h).into()).collect(),
                ))
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::TransactionsByHash {
                        transactions: Vec::with_capacity(hashes.len()),
                        hashes,
                    },
                    tx,
                    span,
                }),
            (AwaitingRequest, FindBlocks { known_blocks, stop }) => self
                .peer_tx
                .send(Message::GetBlocks {
                    block_locator_hashes: known_blocks,
                    hash_stop: stop.unwrap_or(block::Hash([0; 32])),
                })
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::FindBlocks,
                    tx,
                    span,
                }),
            (AwaitingRequest, PushTransaction(transaction)) => {
                // Since we're not waiting for further messages, we need to
                // send a response before dropping tx.
                let _ = tx.send(Ok(Response::Nil));
                self.peer_tx
                    .send(Message::Tx(transaction))
                    .await
                    .map_err(|e| e.into())
                    .map(|()| AwaitingRequest)
            }
            (AwaitingRequest, AdvertiseTransactions(hashes)) => {
                let _ = tx.send(Ok(Response::Nil));
                self.peer_tx
                    .send(Message::Inv(hashes.iter().map(|h| (*h).into()).collect()))
                    .await
                    .map_err(|e| e.into())
                    .map(|()| AwaitingRequest)
            }
            (AwaitingRequest, AdvertiseBlock(hash)) => {
                let _ = tx.send(Ok(Response::Nil));
                self.peer_tx
                    .send(Message::Inv(vec![hash.into()]))
                    .await
                    .map_err(|e| e.into())
                    .map(|()| AwaitingRequest)
            }
        } {
            Ok(new_state) => {
                self.state = new_state;
                self.request_timer = Some(delay_for(constants::REQUEST_TIMEOUT));
            }
            Err(e) => self.fail_with(e),
        }
    }

    // This function has its own span, because we're creating a new work
    // context (namely, the work of processing the inbound msg as a request)
    #[instrument(skip(self))]
    async fn handle_message_as_request(&mut self, msg: Message) {
        // XXX(hdevalence) -- using multiple match statements here
        // prevents us from having exhaustiveness checking.
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
                trace!(?nonce, "responding to heartbeat");
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
            Message::GetData(items)
                if items
                    .iter()
                    .all(|item| matches!(item, InventoryHash::Block(_))) =>
            {
                Some(Request::BlocksByHash(
                    items
                        .iter()
                        .map(|item| {
                            if let InventoryHash::Block(hash) = item {
                                *hash
                            } else {
                                unreachable!("already checked all items are InventoryHash::Block")
                            }
                        })
                        .collect(),
                ))
            }
            Message::GetData(items)
                if items
                    .iter()
                    .all(|item| matches!(item, InventoryHash::Tx(_))) =>
            {
                Some(Request::TransactionsByHash(
                    items
                        .iter()
                        .map(|item| {
                            if let InventoryHash::Tx(hash) = item {
                                *hash
                            } else {
                                unreachable!("already checked all items are InventoryHash::Tx")
                            }
                        })
                        .collect(),
                ))
            }
            Message::GetData(items) => {
                debug!(?items, "could not interpret getdata message");
                None
            }
            Message::Tx(transaction) => Some(Request::PushTransaction(transaction)),
            // We don't expect to be advertised multiple blocks at a time,
            // so we ignore any advertisements of multiple blocks.
            Message::Inv(items)
                if items.len() == 1 && matches!(items[0], InventoryHash::Block(_)) =>
            {
                if let InventoryHash::Block(hash) = items[0] {
                    Some(Request::AdvertiseBlock(hash))
                } else {
                    unreachable!("already checked we got a single block hash");
                }
            }
            // This match arm is terrible, because we have to check that all the items
            // are the correct kind and *then* convert them all.
            Message::Inv(items)
                if items
                    .iter()
                    .all(|item| matches!(item, InventoryHash::Tx(_))) =>
            {
                Some(Request::AdvertiseTransactions(
                    items
                        .iter()
                        .map(|item| {
                            if let InventoryHash::Tx(hash) = item {
                                *hash
                            } else {
                                unreachable!("already checked all items are InventoryHash::Tx")
                            }
                        })
                        .collect(),
                ))
            }
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
            Response::Transactions(transactions) => {
                // Generate one tx message per transaction.
                for transaction in transactions.into_iter() {
                    if let Err(e) = self.peer_tx.send(Message::Tx(transaction)).await {
                        self.fail_with(e.into());
                    }
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
            Response::BlockHashes(hashes) => {
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
