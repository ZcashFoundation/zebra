//! Zcash peer connection protocol handing for Zebra.
//!
//! Maps the external Zcash/Bitcoin protocol to Zebra's internal request/response
//! protocol.
//!
//! This module contains a lot of undocumented state, assumptions and invariants.
//! And it's unclear if these assumptions match the `zcashd` implementation.
//! It should be refactored into a cleaner set of request/response pairs (#1515).

use std::collections::HashSet;
use std::sync::Arc;

use futures::{
    channel::{mpsc, oneshot},
    future::{self, Either},
    prelude::*,
    stream::Stream,
};
use tokio::time::{sleep, Sleep};
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
    BoxError,
};

use super::{ClientRequest, ErrorSlot, PeerError, SharedPeerError};

pub(super) enum Handler {
    /// Indicates that the handler has finished processing the request.
    /// An error here is scoped to the request.
    Finished(Result<Response, PeerError>),
    Ping(Nonce),
    Peers,
    FindBlocks,
    FindHeaders,
    BlocksByHash {
        hashes: HashSet<block::Hash>,
        blocks: Vec<Arc<Block>>,
    },
    TransactionsByHash {
        hashes: HashSet<transaction::Hash>,
        transactions: Vec<Arc<Transaction>>,
    },
    MempoolTransactions,
}

impl Handler {
    /// Try to handle `msg` as a response to a client request, possibly consuming
    /// it in the process.
    ///
    /// This function is where we statefully interpret Bitcoin/Zcash messages
    /// into responses to messages in the internal request/response protocol.
    /// This conversion is done by a sequence of (request, message) match arms,
    /// each of which contains the conversion logic for that pair.
    ///
    /// Taking ownership of the message means that we can pass ownership of its
    /// contents to responses without additional copies.  If the message is not
    /// interpretable as a response, we return ownership to the caller.
    ///
    /// Unexpected messages are left unprocessed, and may be rejected later.
    fn process_message(&mut self, msg: Message) -> Option<Message> {
        let mut ignored_msg = None;
        // XXX can this be avoided?
        let tmp_state = std::mem::replace(self, Handler::Finished(Ok(Response::Nil)));

        *self = match (tmp_state, msg) {
            (Handler::Ping(req_nonce), Message::Pong(rsp_nonce)) => {
                if req_nonce == rsp_nonce {
                    Handler::Finished(Ok(Response::Nil))
                } else {
                    Handler::Ping(req_nonce)
                }
            }
            (Handler::Peers, Message::Addr(addrs)) => Handler::Finished(Ok(Response::Peers(addrs))),
            // `zcashd` returns requested transactions in a single batch of messages.
            // Other transaction or non-transaction messages can come before or after the batch.
            // After the transaction batch, `zcashd` sends `NotFound` if any transactions are missing:
            // https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5617
            (
                Handler::TransactionsByHash {
                    mut hashes,
                    mut transactions,
                },
                Message::Tx(transaction),
            ) => {
                // assumptions:
                //   - the transaction list is contiguous
                //   - missing transaction hashes are included in a `NotFound` message
                if hashes.remove(&transaction.hash()) {
                    // we are in the middle of the contiguous transaction response
                    transactions.push(transaction);
                    if hashes.is_empty() {
                        Handler::Finished(Ok(Response::Transactions(transactions)))
                    } else {
                        Handler::TransactionsByHash {
                            hashes,
                            transactions,
                        }
                    }
                } else {
                    // we are before or after the contiguous block response
                    ignored_msg = Some(Message::Tx(transaction));
                    if !transactions.is_empty() {
                        // we don't expect zcashd to behave like this
                        error!("unexpected transaction from peer: transaction responses should be contiguous, followed by notfound. Using partial received transactions as the peer response");
                        // TODO: does the caller need a list of missing transactions?
                        Handler::Finished(Ok(Response::Transactions(transactions)))
                    } else {
                        // If the caller doesn't know any of the transactions,
                        // we'll get a `NotFound` with all the hashes
                        Handler::TransactionsByHash {
                            hashes,
                            transactions,
                        }
                    }
                }
            }
            // `zcashd` peers actually return this response
            (
                Handler::TransactionsByHash {
                    hashes,
                    transactions,
                },
                Message::NotFound(items),
            ) => {
                // assumptions:
                //   - the peer eventually returns a transaction or a `NotFound` entry
                //     for each hash
                //   - all `NotFound` entries are contained in a single message
                //   - the `NotFound` message comes after the transactions
                let hash_in_hashes = |item: &InventoryHash| {
                    if let InventoryHash::Tx(hash) = item {
                        hashes.contains(hash)
                    } else {
                        false
                    }
                };
                if items.iter().all(hash_in_hashes) {
                    // all hashes have a transaction or a `NotFound` entry
                    if !transactions.is_empty() {
                        // TODO: does the caller need a list of missing transactions?
                        Handler::Finished(Ok(Response::Transactions(transactions)))
                    } else {
                        Handler::Finished(Err(PeerError::NotFound(items)))
                    }
                } else {
                    // waiting for more transactions or a complete `NotFound` message
                    if items.iter().any(hash_in_hashes) {
                        debug!(
                            not_found_transactions = ?items.iter().cloned().filter(hash_in_hashes).count(),
                            remaining_transactions = ?hashes.len(),
                            "notfound containing some of the remaining requested transactions");
                    }
                    ignored_msg = Some(Message::NotFound(items));
                    Handler::TransactionsByHash {
                        hashes,
                        transactions,
                    }
                }
            }
            // `zcashd` returns requested blocks in a single batch of messages.
            // Other blocks or non-blocks messages can come before or after the batch.
            // `zcashd` silently skips missing blocks, rather than sending a final `NotFound` message.
            // https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5523
            (
                Handler::BlocksByHash {
                    mut hashes,
                    mut blocks,
                },
                Message::Block(block),
            ) => {
                // assumptions:
                //   - the block list is contiguous
                //   - missing blocks are silently skipped (rather than using `NotFound`)
                if hashes.remove(&block.hash()) {
                    // we are in the middle of the contiguous block response
                    blocks.push(block);
                    if hashes.is_empty() {
                        Handler::Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        Handler::BlocksByHash { hashes, blocks }
                    }
                } else {
                    // we are before or after the contiguous block response
                    ignored_msg = Some(Message::Block(block));
                    if !blocks.is_empty() {
                        // TODO: does the caller need a list of missing blocks?
                        Handler::Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        // TODO: what if the peer doesn't know any of the blocks
                        //       we asked for?
                        Handler::BlocksByHash { hashes, blocks }
                    }
                }
            }
            // peers are allowed to return this response, but `zcashd` never does
            (Handler::BlocksByHash { hashes, blocks }, Message::NotFound(items)) => {
                // assumptions:
                //   - the peer eventually returns a block or a `NotFound` entry
                //     for each hash
                //   - all `NotFound` entries are contained in a single message
                //   - the `NotFound` message comes after the blocks
                let hash_in_hashes = |item: &InventoryHash| {
                    if let InventoryHash::Block(hash) = item {
                        hashes.contains(hash)
                    } else {
                        false
                    }
                };
                if items.iter().all(hash_in_hashes) {
                    // all hashes have a block or a `NotFound` entry
                    if !blocks.is_empty() {
                        // TODO: does the caller need a list of missing blocks?
                        Handler::Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        Handler::Finished(Err(PeerError::NotFound(items)))
                    }
                } else {
                    // waiting for more blocks or a complete `NotFound` message
                    if items.iter().any(hash_in_hashes) {
                        debug!(
                            not_found_blocks = ?items.iter().cloned().filter(hash_in_hashes).count(),
                            remaining_blocks = ?hashes.len(),
                            "notfound containing some of the remaining requested blocks");
                    }
                    ignored_msg = Some(Message::NotFound(items));
                    Handler::BlocksByHash { hashes, blocks }
                }
            }
            (Handler::FindBlocks, Message::Inv(items))
                if items
                    .iter()
                    .all(|item| matches!(item, InventoryHash::Block(_))) =>
            {
                Handler::Finished(Ok(Response::BlockHashes(
                    block_hashes(&items[..]).collect(),
                )))
            }
            (Handler::MempoolTransactions, Message::Inv(items))
                if items
                    .iter()
                    .all(|item| matches!(item, InventoryHash::Tx(_))) =>
            {
                Handler::Finished(Ok(Response::TransactionHashes(
                    transaction_hashes(&items[..]).collect(),
                )))
            }
            (Handler::FindHeaders, Message::Headers(headers)) => {
                Handler::Finished(Ok(Response::BlockHeaders(headers)))
            }
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
    pub(super) request_timer: Option<Sleep>,
    pub(super) svc: S,
    pub(super) client_rx: mpsc::Receiver<ClientRequest>,
    /// A slot for an error shared between the Connection and the Client that uses it.
    pub(super) error_slot: ErrorSlot,
    //pub(super) peer_rx: Rx,
    pub(super) peer_tx: Tx,
}

impl<S, Tx> Connection<S, Tx>
where
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Error: Into<BoxError>,
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
                // so wait on either a peer message, or on a request cancellation.
                State::AwaitingResponse {
                    ref span,
                    ref mut tx,
                    ..
                } => {
                    // we have to get rid of the span reference so we can tamper with the state
                    let span = span.clone();
                    trace!(parent: &span, "awaiting response to client request");
                    let timer_ref = self
                        .request_timer
                        .as_mut()
                        .expect("timeout must be set while awaiting response");
                    let cancel = future::select(timer_ref, tx.cancellation());
                    match future::select(peer_rx.next(), cancel)
                        .instrument(span.clone())
                        .await
                    {
                        Either::Left((None, _)) => self.fail_with(PeerError::ConnectionClosed),
                        Either::Left((Some(Err(e)), _)) => self.fail_with(e.into()),
                        Either::Left((Some(Ok(peer_msg)), _cancel)) => {
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
                                        let _ = tx.send(response.map_err(Into::into));
                                        State::AwaitingRequest
                                    }
                                    pending @ State::AwaitingResponse { .. } => pending,
                                    _ => unreachable!(),
                                };
                            }
                        }
                        Either::Right((Either::Left(_), _peer_fut)) => {
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
                        Either::Right((Either::Right(_), _peer_fut)) => {
                            trace!(parent: &span, "client request was cancelled");
                            self.state = State::AwaitingRequest;
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

        if tx.is_canceled() {
            metrics::counter!("peer.canceled", 1);
            tracing::debug!("ignoring canceled request");
            return;
        }

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
                .send(Message::GetBlocks { known_blocks, stop })
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::FindBlocks,
                    tx,
                    span,
                }),
            (AwaitingRequest, FindHeaders { known_blocks, stop }) => self
                .peer_tx
                .send(Message::GetHeaders { known_blocks, stop })
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::FindHeaders,
                    tx,
                    span,
                }),
            (AwaitingRequest, MempoolTransactions) => self
                .peer_tx
                .send(Message::Mempool)
                .await
                .map_err(|e| e.into())
                .map(|()| AwaitingResponse {
                    handler: Handler::MempoolTransactions,
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
                self.request_timer = Some(sleep(constants::REQUEST_TIMEOUT));
            }
            Err(e) => self.fail_with(e),
        }
    }

    // This function has its own span, because we're creating a new work
    // context (namely, the work of processing the inbound msg as a request)
    #[instrument(name = "msg_as_req", skip(self, msg), fields(%msg))]
    async fn handle_message_as_request(&mut self, msg: Message) {
        trace!(?msg);
        let req = match msg {
            Message::Ping(nonce) => {
                trace!(?nonce, "responding to heartbeat");
                if let Err(e) = self.peer_tx.send(Message::Pong(nonce)).await {
                    self.fail_with(e.into());
                }
                return;
            }
            // These messages shouldn't be sent outside of a handshake.
            Message::Version { .. } => {
                self.fail_with(PeerError::DuplicateHandshake);
                return;
            }
            Message::Verack { .. } => {
                self.fail_with(PeerError::DuplicateHandshake);
                return;
            }
            // These messages should already be handled as a response if they
            // could be a response, so if we see them here, they were either
            // sent unsolicited, or they were sent in response to a canceled request
            // that we've already forgotten about.
            Message::Reject { .. } => {
                tracing::debug!("got reject message unsolicited or from canceled request");
                return;
            }
            Message::NotFound { .. } => {
                tracing::debug!("got notfound message unsolicited or from canceled request");
                return;
            }
            Message::Pong(_) => {
                tracing::debug!("got pong message unsolicited or from canceled request");
                return;
            }
            Message::Block(_) => {
                tracing::debug!("got block message unsolicited or from canceled request");
                return;
            }
            Message::Headers(_) => {
                tracing::debug!("got headers message unsolicited or from canceled request");
                return;
            }
            // These messages should never be sent by peers.
            Message::FilterLoad { .. }
            | Message::FilterAdd { .. }
            | Message::FilterClear { .. } => {
                self.fail_with(PeerError::UnsupportedMessage(
                    "got BIP11 message without advertising NODE_BLOOM",
                ));
                return;
            }
            // Zebra crawls the network proactively, to prevent
            // peers from inserting data into our address book.
            Message::Addr(_) => {
                trace!("ignoring unsolicited addr message");
                return;
            }
            Message::Tx(transaction) => Request::PushTransaction(transaction),
            Message::Inv(items) => match &items[..] {
                // We don't expect to be advertised multiple blocks at a time,
                // so we ignore any advertisements of multiple blocks.
                [InventoryHash::Block(hash)] => Request::AdvertiseBlock(*hash),
                [InventoryHash::Tx(_), rest @ ..]
                    if rest.iter().all(|item| matches!(item, InventoryHash::Tx(_))) =>
                {
                    Request::TransactionsByHash(transaction_hashes(&items).collect())
                }
                _ => {
                    self.fail_with(PeerError::WrongMessage("inv with mixed item types"));
                    return;
                }
            },
            Message::GetData(items) => match &items[..] {
                [InventoryHash::Block(_), rest @ ..]
                    if rest
                        .iter()
                        .all(|item| matches!(item, InventoryHash::Block(_))) =>
                {
                    Request::BlocksByHash(block_hashes(&items).collect())
                }
                [InventoryHash::Tx(_), rest @ ..]
                    if rest.iter().all(|item| matches!(item, InventoryHash::Tx(_))) =>
                {
                    Request::TransactionsByHash(transaction_hashes(&items).collect())
                }
                _ => {
                    self.fail_with(PeerError::WrongMessage("getdata with mixed item types"));
                    return;
                }
            },
            Message::GetAddr => Request::Peers,
            Message::GetBlocks { known_blocks, stop } => Request::FindBlocks { known_blocks, stop },
            Message::GetHeaders { known_blocks, stop } => {
                Request::FindHeaders { known_blocks, stop }
            }
            Message::Mempool => Request::MempoolTransactions,
        };

        self.drive_peer_request(req).await
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
                    tracing::warn!("inbound service is overloaded, closing connection");
                    metrics::counter!("pool.closed.loadshed", 1);
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
            Response::BlockHeaders(headers) => {
                if let Err(e) = self.peer_tx.send(Message::Headers(headers)).await {
                    self.fail_with(e.into())
                }
            }
            Response::TransactionHashes(hashes) => {
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

fn transaction_hashes(items: &'_ [InventoryHash]) -> impl Iterator<Item = transaction::Hash> + '_ {
    items.iter().filter_map(|item| {
        if let InventoryHash::Tx(hash) = item {
            Some(*hash)
        } else {
            None
        }
    })
}

fn block_hashes(items: &'_ [InventoryHash]) -> impl Iterator<Item = block::Hash> + '_ {
    items.iter().filter_map(|item| {
        if let InventoryHash::Block(hash) = item {
            Some(*hash)
        } else {
            None
        }
    })
}
