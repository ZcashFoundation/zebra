//! Zcash peer connection protocol handing for Zebra.
//!
//! Maps the external Zcash/Bitcoin protocol to Zebra's internal request/response
//! protocol.
//!
//! This module contains a lot of undocumented state, assumptions and invariants.
//! And it's unclear if these assumptions match the `zcashd` implementation.
//! It should be refactored into a cleaner set of request/response pairs (#1515).

use std::{collections::HashSet, sync::Arc};

use futures::{
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
    transaction::{UnminedTx, UnminedTxId},
};

use crate::{
    constants,
    protocol::{
        external::{types::Nonce, InventoryHash, Message},
        internal::{Request, Response},
    },
    BoxError,
};

use super::{
    ClientRequestReceiver, ErrorSlot, InProgressClientRequest, MustUseOneshotSender, PeerError,
    SharedPeerError,
};

#[derive(Debug)]
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
    TransactionsById {
        pending_ids: HashSet<UnminedTxId>,
        transactions: Vec<UnminedTx>,
    },
    MempoolTransactionIds,
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
                Handler::TransactionsById {
                    mut pending_ids,
                    mut transactions,
                },
                Message::Tx(transaction),
            ) => {
                // assumptions:
                //   - the transaction messages are sent in a single continous batch
                //   - missing transaction hashes are included in a `NotFound` message
                if pending_ids.remove(&transaction.id) {
                    // we are in the middle of the continous transaction messages
                    transactions.push(transaction);
                    if pending_ids.is_empty() {
                        Handler::Finished(Ok(Response::Transactions(transactions)))
                    } else {
                        Handler::TransactionsById {
                            pending_ids,
                            transactions,
                        }
                    }
                } else {
                    // We got a transaction we didn't ask for. If the caller doesn't know any of the
                    // transactions, they should have sent a `NotFound` with all the hashes, rather
                    // than an unsolicited transaction.
                    //
                    // So either:
                    // 1. The peer implements the protocol badly, skipping `NotFound`.
                    //    We should cancel the request, so we don't hang waiting for transactions
                    //    that will never arrive.
                    // 2. The peer sent an unsolicited transaction.
                    //    We should ignore the transaction, and wait for the actual response.
                    //
                    // We end the request, so we don't hang on bad peers (case 1). But we keep the
                    // connection open, so the inbound service can process transactions from good
                    // peers (case 2).
                    ignored_msg = Some(Message::Tx(transaction));
                    if !transactions.is_empty() {
                        // if our peers start sending mixed solicited and unsolicited transactions,
                        // we should update this code to handle those responses
                        error!("unexpected transaction from peer: transaction responses should be sent in a continuous batch, followed by notfound. Using partial received transactions as the peer response");
                        // TODO: does the caller need a list of missing transactions? (#1515)
                        Handler::Finished(Ok(Response::Transactions(transactions)))
                    } else {
                        // TODO: is it really an error if we ask for a transaction hash, but the peer
                        // doesn't know it? Should we close the connection on that kind of error?
                        // Should we fake a NotFound response here? (#1515)
                        let missing_transaction_ids = pending_ids.iter().map(Into::into).collect();
                        Handler::Finished(Err(PeerError::NotFound(missing_transaction_ids)))
                    }
                }
            }
            // `zcashd` peers actually return this response
            (
                Handler::TransactionsById {
                    pending_ids,
                    transactions,
                },
                Message::NotFound(missing_invs),
            ) => {
                // assumptions:
                //   - the peer eventually returns a transaction or a `NotFound` entry
                //     for each hash
                //   - all `NotFound` entries are contained in a single message
                //   - the `NotFound` message comes after the transaction messages
                //
                // If we're in sync with the peer, then the `NotFound` should contain the remaining
                // hashes from the handler. If we're not in sync with the peer, we should return
                // what we got so far, and log an error.
                let missing_transaction_ids: HashSet<_> = transaction_ids(&missing_invs).collect();
                if missing_transaction_ids != pending_ids {
                    trace!(?missing_invs, ?missing_transaction_ids, ?pending_ids);
                    // if these errors are noisy, we should replace them with debugs
                    error!("unexpected notfound message from peer: all remaining transaction hashes should be listed in the notfound. Using partial received transactions as the peer response");
                }
                if missing_transaction_ids.len() != missing_invs.len() {
                    trace!(?missing_invs, ?missing_transaction_ids, ?pending_ids);
                    error!("unexpected notfound message from peer: notfound contains duplicate hashes or non-transaction hashes. Using partial received transactions as the peer response");
                }

                if !transactions.is_empty() {
                    // TODO: does the caller need a list of missing transactions? (#1515)
                    Handler::Finished(Ok(Response::Transactions(transactions)))
                } else {
                    // TODO: is it really an error if we ask for a transaction hash, but the peer
                    // doesn't know it? Should we close the connection on that kind of error? (#1515)
                    Handler::Finished(Err(PeerError::NotFound(missing_invs)))
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
                //   - the block messages are sent in a single continuous batch
                //   - missing blocks are silently skipped
                //     (there is no `NotFound` message at the end of the batch)
                if hashes.remove(&block.hash()) {
                    // we are in the middle of the continuous block messages
                    blocks.push(block);
                    if hashes.is_empty() {
                        Handler::Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        Handler::BlocksByHash { hashes, blocks }
                    }
                } else {
                    // We got a block we didn't ask for.
                    //
                    // So either:
                    // 1. The peer doesn't know any of the blocks we asked for.
                    //    We should cancel the request, so we don't hang waiting for blocks that
                    //    will never arrive.
                    // 2. The peer sent an unsolicited block.
                    //    We should ignore that block, and wait for the actual response.
                    //
                    // We end the request, so we don't hang on forked or lagging peers (case 1).
                    // But we keep the connection open, so the inbound service can process blocks
                    // from good peers (case 2).
                    ignored_msg = Some(Message::Block(block));
                    if !blocks.is_empty() {
                        // TODO: does the caller need a list of missing blocks? (#1515)
                        Handler::Finished(Ok(Response::Blocks(blocks)))
                    } else {
                        // TODO: is it really an error if we ask for a block hash, but the peer
                        // doesn't know it? Should we close the connection on that kind of error?
                        // Should we fake a NotFound response here? (#1515)
                        let items = hashes.iter().map(|h| InventoryHash::Block(*h)).collect();
                        Handler::Finished(Err(PeerError::NotFound(items)))
                    }
                }
            }
            // peers are allowed to return this response, but `zcashd` never does
            (Handler::BlocksByHash { hashes, blocks }, Message::NotFound(items)) => {
                // assumptions:
                //   - the peer eventually returns a block or a `NotFound` entry
                //     for each hash
                //   - all `NotFound` entries are contained in a single message
                //   - the `NotFound` message comes after the block messages
                //
                // If we're in sync with the peer, then the `NotFound` should contain the remaining
                // hashes from the handler. If we're not in sync with the peer, we should return
                // what we got so far, and log an error.
                let missing_blocks: HashSet<_> = items
                    .iter()
                    .filter_map(|inv| match &inv {
                        InventoryHash::Block(b) => Some(b),
                        _ => None,
                    })
                    .cloned()
                    .collect();
                if missing_blocks != hashes {
                    trace!(?items, ?missing_blocks, ?hashes);
                    // if these errors are noisy, we should replace them with debugs
                    error!("unexpected notfound message from peer: all remaining block hashes should be listed in the notfound. Using partial received blocks as the peer response");
                }
                if missing_blocks.len() != items.len() {
                    trace!(?items, ?missing_blocks, ?hashes);
                    error!("unexpected notfound message from peer: notfound contains duplicate hashes or non-block hashes. Using partial received blocks as the peer response");
                }

                if !blocks.is_empty() {
                    // TODO: does the caller need a list of missing blocks? (#1515)
                    Handler::Finished(Ok(Response::Blocks(blocks)))
                } else {
                    // TODO: is it really an error if we ask for a block hash, but the peer
                    // doesn't know it? Should we close the connection on that kind of error? (#1515)
                    Handler::Finished(Err(PeerError::NotFound(items)))
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
            (Handler::MempoolTransactionIds, Message::Inv(items))
                if items.iter().all(|item| item.unmined_tx_id().is_some()) =>
            {
                Handler::Finished(Ok(Response::TransactionIds(
                    transaction_ids(&items).collect(),
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

#[derive(Debug)]
#[must_use = "AwaitingResponse.tx.send() must be called before drop"]
pub(super) enum State {
    /// Awaiting a client request or a peer message.
    AwaitingRequest,
    /// Awaiting a peer message we can interpret as a client request.
    AwaitingResponse {
        handler: Handler,
        tx: MustUseOneshotSender<Result<Response, SharedPeerError>>,
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
    /// A `mpsc::Receiver<ClientRequest>` that converts its results to
    /// `InProgressClientRequest`
    pub(super) client_rx: ClientRequestReceiver,
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
                    // CORRECTNESS
                    //
                    // Currently, select prefers the first future if multiple
                    // futures are ready.
                    //
                    // The peer can starve client requests if it sends an
                    // uninterrupted series of messages. But this is unlikely in
                    // practice, due to network delays.
                    //
                    // If both futures are ready, there's no particular reason
                    // to prefer one over the other.
                    //
                    // TODO: use `futures::select!`, which chooses a ready future
                    //       at random, avoiding starvation
                    //       (To use `select!`, we'll need to map the different
                    //       results to a new enum types.)
                    match future::select(peer_rx.next(), self.client_rx.next()).await {
                        Either::Left((None, _)) => {
                            self.fail_with(PeerError::ConnectionClosed);
                        }
                        Either::Left((Some(Err(e)), _)) => self.fail_with(e),
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
                    // CORRECTNESS
                    //
                    // Currently, select prefers the first future if multiple
                    // futures are ready.
                    //
                    // If multiple futures are ready, we want the cancellation
                    // to take priority, then the timeout, then peer responses.
                    let cancel = future::select(tx.cancellation(), timer_ref);
                    match future::select(cancel, peer_rx.next())
                        .instrument(span.clone())
                        .await
                    {
                        Either::Right((None, _)) => self.fail_with(PeerError::ConnectionClosed),
                        Either::Right((Some(Err(e)), _)) => self.fail_with(e),
                        Either::Right((Some(Ok(peer_msg)), _cancel)) => {
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
                                _ => unreachable!("unexpected state after AwaitingResponse: {:?}, peer_msg: {:?}, client_receiver: {:?}",
                                                  self.state,
                                                  peer_msg,
                                                  self.client_rx,
                                ),
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
                                    _ => unreachable!(
                                        "unexpected failed connection state while AwaitingResponse: client_receiver: {:?}",
                                        self.client_rx
                                    ),
                                };
                            }
                        }
                        Either::Left((Either::Right(_), _peer_fut)) => {
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
                                _ => unreachable!(
                                    "unexpected failed connection state while AwaitingResponse: client_receiver: {:?}",
                                    self.client_rx
                                ),
                            };
                        }
                        Either::Left((Either::Left(_), _peer_fut)) => {
                            trace!(parent: &span, "client request was cancelled");
                            self.state = State::AwaitingRequest;
                        }
                    }
                }
                // We've failed, but we need to flush all pending client
                // requests before we can return and complete the future.
                State::Failed => {
                    match self.client_rx.next().await {
                        Some(InProgressClientRequest { tx, span, .. }) => {
                            trace!(
                                parent: &span,
                                "sending an error response to a pending request on a failed connection"
                            );
                            // Correctness
                            //
                            // Error slots use a threaded `std::sync::Mutex`, so
                            // accessing the slot can block the async task's
                            // current thread. So we only hold the lock for long
                            // enough to get a reference to the error.
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
    ///
    /// # Panics
    ///
    /// If `self` has already failed with a previous error.
    fn fail_with<E>(&mut self, e: E)
    where
        E: Into<SharedPeerError>,
    {
        let e = e.into();
        debug!(%e,
               connection_state = ?self.state,
               client_receiver = ?self.client_rx,
               "failing peer service with error");

        // Update the shared error slot
        //
        // # Correctness
        //
        // Error slots use a threaded `std::sync::Mutex`, so accessing the slot
        // can block the async task's current thread. We only perform a single
        // slot update per `Client`, and panic to enforce this constraint.
        //
        // This assertion typically fails due to these bugs:
        // * we mark a connection as failed without using fail_with
        // * we call fail_with without checking for a failed connection
        //   state
        // * we continue processing messages after calling fail_with
        //
        // See the original bug #1510 and PR #1531, and the later bug #1599
        // and PR #1600.
        assert!(
            self.error_slot.try_update_error(e).is_ok(),
            "calling fail_with on already-failed connection state: failed connections must stop processing pending requests and responses, then close the connection. state: {:?} client receiver: {:?}",
            self.state,
            self.client_rx
        );

        // We want to close the client channel and set State::Failed so
        // that we can flush any pending client requests. However, we may have
        // an outstanding client request in State::AwaitingResponse, so
        // we need to deal with it first if it exists.
        self.client_rx.close();
        let old_state = std::mem::replace(&mut self.state, State::Failed);
        if let State::AwaitingResponse { tx, .. } = old_state {
            // # Correctness
            //
            // We know the slot has Some(e) because we just set it above,
            // and the error slot is never unset.
            //
            // Accessing the error slot locks a threaded std::sync::Mutex, which
            // can block the current async task thread. We briefly lock the mutex
            // to get a reference to the error.
            let e = self.error_slot.try_get_error().unwrap();
            let _ = tx.send(Err(e));
        }
    }

    /// Handle an incoming client request, possibly generating outgoing messages to the
    /// remote peer.
    ///
    /// NOTE: the caller should use .instrument(msg.span) to instrument the function.
    async fn handle_client_request(&mut self, req: InProgressClientRequest) {
        trace!(?req.request);
        use Request::*;
        use State::*;
        let InProgressClientRequest { request, tx, span } = req;

        if tx.is_canceled() {
            metrics::counter!("peer.canceled", 1);
            tracing::debug!("ignoring canceled request");
            return;
        }

        // These matches return a Result with (new_state, Option<Sender>) or an (error, Sender)
        let new_state_result = match (&self.state, request) {
            (Failed, request) => panic!(
                "failed connection cannot handle new request: {:?}, client_receiver: {:?}",
                request,
                self.client_rx
            ),
            (pending @ AwaitingResponse { .. }, request) => panic!(
                "tried to process new request: {:?} while awaiting a response: {:?}, client_receiver: {:?}",
                request,
                pending,
                self.client_rx
            ),
            (AwaitingRequest, Peers) => match self.peer_tx.send(Message::GetAddr).await {
                Ok(()) => Ok((
                    AwaitingResponse {
                        handler: Handler::Peers,
                        tx,
                        span,
                    },
                    None,
                )),
                Err(e) => Err((e, tx)),
            },
            (AwaitingRequest, Ping(nonce)) => match self.peer_tx.send(Message::Ping(nonce)).await {
                Ok(()) => Ok((
                    AwaitingResponse {
                        handler: Handler::Ping(nonce),
                        tx,
                        span,
                    },
                    None,
                )),
                Err(e) => Err((e, tx)),
            },
            (AwaitingRequest, BlocksByHash(hashes)) => {
                match self
                    .peer_tx
                    .send(Message::GetData(
                        hashes.iter().map(|h| (*h).into()).collect(),
                    ))
                    .await
                {
                    Ok(()) => Ok((
                        AwaitingResponse {
                            handler: Handler::BlocksByHash {
                                blocks: Vec::with_capacity(hashes.len()),
                                hashes,
                            },
                            tx,
                            span,
                        },
                        None,
                    )),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, TransactionsById(ids)) => {
                match self
                    .peer_tx
                    .send(Message::GetData(
                        ids.iter().map(Into::into).collect(),
                    ))
                    .await
                {
                    Ok(()) => Ok((
                        AwaitingResponse {
                            handler: Handler::TransactionsById {
                                transactions: Vec::with_capacity(ids.len()),
                                pending_ids: ids,
                            },
                            tx,
                            span,
                        },
                        None,
                    )),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, FindBlocks { known_blocks, stop }) => {
                match self
                    .peer_tx
                    .send(Message::GetBlocks { known_blocks, stop })
                    .await
                {
                    Ok(()) => Ok((
                        AwaitingResponse {
                            handler: Handler::FindBlocks,
                            tx,
                            span,
                        },
                        None,
                    )),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, FindHeaders { known_blocks, stop }) => {
                match self
                    .peer_tx
                    .send(Message::GetHeaders { known_blocks, stop })
                    .await
                {
                    Ok(()) => Ok((
                        AwaitingResponse {
                            handler: Handler::FindHeaders,
                            tx,
                            span,
                        },
                        None,
                    )),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, MempoolTransactionIds) => {
                match self.peer_tx.send(Message::Mempool).await {
                    Ok(()) => Ok((
                        AwaitingResponse {
                            handler: Handler::MempoolTransactionIds,
                            tx,
                            span,
                        },
                        None,
                    )),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, PushTransaction(transaction)) => {
                match self.peer_tx.send(Message::Tx(transaction)).await {
                    Ok(()) => Ok((AwaitingRequest, Some(tx))),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, AdvertiseTransactionIds(hashes)) => {
                match self
                    .peer_tx
                    .send(Message::Inv(hashes.iter().map(|h| (*h).into()).collect()))
                    .await
                {
                    Ok(()) => Ok((AwaitingRequest, Some(tx))),
                    Err(e) => Err((e, tx)),
                }
            }
            (AwaitingRequest, AdvertiseBlock(hash)) => {
                match self.peer_tx.send(Message::Inv(vec![hash.into()])).await {
                    Ok(()) => Ok((AwaitingRequest, Some(tx))),
                    Err(e) => Err((e, tx)),
                }
            }
        };
        // Updates state or fails. Sends the error on the Sender if it is Some.
        match new_state_result {
            Ok((AwaitingRequest, Some(tx))) => {
                // Since we're not waiting for further messages, we need to
                // send a response before dropping tx.
                let _ = tx.send(Ok(Response::Nil));
                self.state = AwaitingRequest;
                self.request_timer = Some(sleep(constants::REQUEST_TIMEOUT));
            }
            Ok((new_state @ AwaitingResponse { .. }, None)) => {
                self.state = new_state;
                self.request_timer = Some(sleep(constants::REQUEST_TIMEOUT));
            }
            Err((e, tx)) => {
                let e = SharedPeerError::from(e);
                let _ = tx.send(Err(e.clone()));
                self.fail_with(e);
            }
            // unreachable states
            Ok((Failed, tx)) => unreachable!(
                "failed client requests must use fail_with(error) to reach a Failed state. tx: {:?}",
                tx
            ),
            Ok((AwaitingRequest, None)) => unreachable!(
                "successful AwaitingRequest states must send a response on tx, but tx is None",
            ),
            Ok((new_state @ AwaitingResponse { .. }, Some(tx))) => unreachable!(
                "successful AwaitingResponse states must keep tx, but tx is Some: {:?} for: {:?}",
                tx, new_state,
            ),
        };
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
                    self.fail_with(e);
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
                    "got BIP111 message without advertising NODE_BLOOM",
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
                tx_ids
                    if tx_ids.iter().all(|item| item.unmined_tx_id().is_some())
                        && !tx_ids.is_empty() =>
                {
                    Request::AdvertiseTransactionIds(transaction_ids(&items).collect())
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
                tx_ids
                    if tx_ids.iter().all(|item| item.unmined_tx_id().is_some())
                        && !tx_ids.is_empty() =>
                {
                    Request::TransactionsById(transaction_ids(&items).collect())
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
            Message::Mempool => Request::MempoolTransactionIds,
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
            // TODO: treat `TryRecvError::Closed` in `Inbound::poll_ready` as a fatal error (#1655)
            self.fail_with(PeerError::Overloaded);
            return;
        }

        let rsp = match self.svc.call(req).await {
            Err(e) => {
                if e.is::<Overloaded>() {
                    tracing::warn!("inbound service is overloaded, closing connection");
                    metrics::counter!("pool.closed.loadshed", 1);
                    self.fail_with(PeerError::Overloaded);
                } else {
                    // We could send a reject to the remote peer, but that might cause
                    // them to disconnect, and we might be using them to sync blocks.
                    // For similar reasons, we don't want to fail_with() here - we
                    // only close the connection if the peer is doing something wrong.
                    error!(%e,
                           connection_state = ?self.state,
                           client_receiver = ?self.client_rx,
                           "error processing peer request");
                }
                return;
            }
            Ok(rsp) => rsp,
        };

        match rsp {
            Response::Nil => { /* generic success, do nothing */ }
            Response::Peers(addrs) => {
                if let Err(e) = self.peer_tx.send(Message::Addr(addrs)).await {
                    self.fail_with(e);
                }
            }
            Response::Transactions(transactions) => {
                // Generate one tx message per transaction.
                for transaction in transactions.into_iter() {
                    if let Err(e) = self.peer_tx.send(Message::Tx(transaction)).await {
                        self.fail_with(e);
                        return;
                    }
                }
            }
            Response::Blocks(blocks) => {
                // Generate one block message per block.
                for block in blocks.into_iter() {
                    if let Err(e) = self.peer_tx.send(Message::Block(block)).await {
                        self.fail_with(e);
                        return;
                    }
                }
            }
            Response::BlockHashes(hashes) => {
                if let Err(e) = self
                    .peer_tx
                    .send(Message::Inv(hashes.into_iter().map(Into::into).collect()))
                    .await
                {
                    self.fail_with(e)
                }
            }
            Response::BlockHeaders(headers) => {
                if let Err(e) = self.peer_tx.send(Message::Headers(headers)).await {
                    self.fail_with(e)
                }
            }
            Response::TransactionIds(hashes) => {
                if let Err(e) = self
                    .peer_tx
                    .send(Message::Inv(hashes.into_iter().map(Into::into).collect()))
                    .await
                {
                    self.fail_with(e)
                }
            }
        }
    }
}

/// Map a list of inventory hashes to the corresponding unmined transaction IDs.
/// Non-transaction inventory hashes are skipped.
///
/// v4 transactions use a legacy transaction ID, and
/// v5 transactions use a witnessed transaction ID.
fn transaction_ids(items: &'_ [InventoryHash]) -> impl Iterator<Item = UnminedTxId> + '_ {
    items.iter().filter_map(InventoryHash::unmined_tx_id)
}

/// Map a list of inventory hashes to the corresponding block hashes.
/// Non-block inventory hashes are skipped.
fn block_hashes(items: &'_ [InventoryHash]) -> impl Iterator<Item = block::Hash> + '_ {
    items.iter().filter_map(|item| {
        if let InventoryHash::Block(hash) = item {
            Some(*hash)
        } else {
            None
        }
    })
}
