//! Zebra's per-peer connection state machine.
//!
//! Maps the external Zcash/Bitcoin protocol to Zebra's internal request/response
//! protocol.
//!
//! This module contains a lot of undocumented state, assumptions and invariants.
//! And it's unclear if these assumptions match the `zcashd` implementation.
//! It should be refactored into a cleaner set of request/response pairs (#1515).

use std::{borrow::Cow, collections::HashSet, fmt, pin::Pin, sync::Arc};

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
    meta_addr::MetaAddr,
    peer::{
        connection::peer_tx::PeerTx, error::AlreadyErrored, ClientRequest, ClientRequestReceiver,
        ConnectionInfo, ErrorSlot, InProgressClientRequest, MustUseClientResponseSender, PeerError,
        SharedPeerError,
    },
    peer_set::ConnectionTracker,
    protocol::{
        external::{types::Nonce, InventoryHash, Message},
        internal::{InventoryResponse, Request, Response},
    },
    BoxError,
};

use InventoryResponse::*;

mod peer_tx;

#[cfg(test)]
mod tests;

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
        pending_hashes: HashSet<block::Hash>,
        blocks: Vec<Arc<Block>>,
    },
    TransactionsById {
        pending_ids: HashSet<UnminedTxId>,
        transactions: Vec<UnminedTx>,
    },
    MempoolTransactionIds,
}

impl fmt::Display for Handler {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Handler::Finished(Ok(response)) => format!("Finished({response})"),
            Handler::Finished(Err(error)) => format!("Finished({error})"),

            Handler::Ping(_) => "Ping".to_string(),
            Handler::Peers => "Peers".to_string(),

            Handler::FindBlocks => "FindBlocks".to_string(),
            Handler::FindHeaders => "FindHeaders".to_string(),
            Handler::BlocksByHash {
                pending_hashes,
                blocks,
            } => format!(
                "BlocksByHash {{ pending_hashes: {}, blocks: {} }}",
                pending_hashes.len(),
                blocks.len()
            ),

            Handler::TransactionsById {
                pending_ids,
                transactions,
            } => format!(
                "TransactionsById {{ pending_ids: {}, transactions: {} }}",
                pending_ids.len(),
                transactions.len()
            ),
            Handler::MempoolTransactionIds => "MempoolTransactionIds".to_string(),
        })
    }
}

impl Handler {
    /// Returns the Zebra internal handler type as a string.
    pub fn command(&self) -> Cow<'static, str> {
        match self {
            Handler::Finished(Ok(response)) => format!("Finished({})", response.command()).into(),
            Handler::Finished(Err(error)) => format!("Finished({})", error.kind()).into(),

            Handler::Ping(_) => "Ping".into(),
            Handler::Peers => "Peers".into(),

            Handler::FindBlocks { .. } => "FindBlocks".into(),
            Handler::FindHeaders { .. } => "FindHeaders".into(),

            Handler::BlocksByHash { .. } => "BlocksByHash".into(),
            Handler::TransactionsById { .. } => "TransactionsById".into(),

            Handler::MempoolTransactionIds => "MempoolTransactionIds".into(),
        }
    }

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

        debug!(handler = %tmp_state, %msg, "received peer response to Zebra request");

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
            // After the transaction batch, `zcashd` sends `notfound` if any transactions are missing:
            // https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5617
            (
                Handler::TransactionsById {
                    mut pending_ids,
                    mut transactions,
                },
                Message::Tx(transaction),
            ) => {
                // assumptions:
                //   - the transaction messages are sent in a single continuous batch
                //   - missing transactions are silently skipped
                //     (there is no `notfound` message at the end of the batch)
                if pending_ids.remove(&transaction.id) {
                    // we are in the middle of the continuous transaction messages
                    transactions.push(transaction);
                } else {
                    // We got a transaction we didn't ask for. If the caller doesn't know any of the
                    // transactions, they should have sent a `notfound` with all the hashes, rather
                    // than an unsolicited transaction.
                    //
                    // So either:
                    // 1. The peer implements the protocol badly, skipping `notfound`.
                    //    We should cancel the request, so we don't hang waiting for transactions
                    //    that will never arrive.
                    // 2. The peer sent an unsolicited transaction.
                    //    We should ignore the transaction, and wait for the actual response.
                    //
                    // We end the request, so we don't hang on bad peers (case 1). But we keep the
                    // connection open, so the inbound service can process transactions from good
                    // peers (case 2).
                    ignored_msg = Some(Message::Tx(transaction));
                }

                if ignored_msg.is_some() && transactions.is_empty() {
                    // If we didn't get anything we wanted, retry the request.
                    let missing_transaction_ids = pending_ids.into_iter().map(Into::into).collect();
                    Handler::Finished(Err(PeerError::NotFoundResponse(missing_transaction_ids)))
                } else if pending_ids.is_empty() || ignored_msg.is_some() {
                    // If we got some of what we wanted, let the internal client know.
                    let available = transactions.into_iter().map(InventoryResponse::Available);
                    let missing = pending_ids.into_iter().map(InventoryResponse::Missing);

                    Handler::Finished(Ok(Response::Transactions(
                        available.chain(missing).collect(),
                    )))
                } else {
                    // Keep on waiting for more.
                    Handler::TransactionsById {
                        pending_ids,
                        transactions,
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
                //   - the peer eventually returns a transaction or a `notfound` entry
                //     for each hash
                //   - all `notfound` entries are contained in a single message
                //   - the `notfound` message comes after the transaction messages
                //
                // If we're in sync with the peer, then the `notfound` should contain the remaining
                // hashes from the handler. If we're not in sync with the peer, we should return
                // what we got so far.
                let missing_transaction_ids: HashSet<_> = transaction_ids(&missing_invs).collect();
                if missing_transaction_ids != pending_ids {
                    trace!(?missing_invs, ?missing_transaction_ids, ?pending_ids);
                    // if these errors are noisy, we should replace them with debugs
                    info!("unexpected notfound message from peer: all remaining transaction hashes should be listed in the notfound. Using partial received transactions as the peer response");
                }
                if missing_transaction_ids.len() != missing_invs.len() {
                    trace!(?missing_invs, ?missing_transaction_ids, ?pending_ids);
                    info!("unexpected notfound message from peer: notfound contains duplicate hashes or non-transaction hashes. Using partial received transactions as the peer response");
                }

                if transactions.is_empty() {
                    // If we didn't get anything we wanted, retry the request.
                    let missing_transaction_ids = pending_ids.into_iter().map(Into::into).collect();
                    Handler::Finished(Err(PeerError::NotFoundResponse(missing_transaction_ids)))
                } else {
                    // If we got some of what we wanted, let the internal client know.
                    let available = transactions.into_iter().map(InventoryResponse::Available);
                    let missing = pending_ids.into_iter().map(InventoryResponse::Missing);

                    Handler::Finished(Ok(Response::Transactions(
                        available.chain(missing).collect(),
                    )))
                }
            }
            // `zcashd` returns requested blocks in a single batch of messages.
            // Other blocks or non-blocks messages can come before or after the batch.
            // `zcashd` silently skips missing blocks, rather than sending a final `notfound` message.
            // https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5523
            (
                Handler::BlocksByHash {
                    mut pending_hashes,
                    mut blocks,
                },
                Message::Block(block),
            ) => {
                // assumptions:
                //   - the block messages are sent in a single continuous batch
                //   - missing blocks are silently skipped
                //     (there is no `notfound` message at the end of the batch)
                if pending_hashes.remove(&block.hash()) {
                    // we are in the middle of the continuous block messages
                    blocks.push(block);
                } else {
                    // We got a block we didn't ask for.
                    //
                    // So either:
                    // 1. The response is for a previously cancelled block request.
                    //    We should treat that block as an inbound gossiped block,
                    //    and wait for the actual response.
                    // 2. The peer doesn't know any of the blocks we asked for.
                    //    We should cancel the request, so we don't hang waiting for blocks that
                    //    will never arrive.
                    // 3. The peer sent an unsolicited block.
                    //    We should treat that block as an inbound gossiped block,
                    //    and wait for the actual response.
                    //
                    // We ignore the message, so we don't desynchronize with the peer. This happens
                    // when we cancel a request and send a second different request, but receive a
                    // response for the first request. If we ended the request then, we could send
                    // a third request to the peer, and end up having to end that request as well
                    // when the response for the second request arrives.
                    //
                    // Ignoring the message gives us a chance to synchronize back to the correct
                    // request. If that doesn't happen, this request times out.
                    //
                    // In case 2, if peers respond with a `notfound` message,
                    // the cascading errors don't happen. The `notfound` message cancels our request,
                    // and we know we are in sync with the peer.
                    //
                    // Zebra sends `notfound` in response to block requests, but `zcashd` doesn't.
                    // So we need this message workaround, and the related inventory workarounds.
                    ignored_msg = Some(Message::Block(block));
                }

                if pending_hashes.is_empty() {
                    // If we got everything we wanted, let the internal client know.
                    let available = blocks.into_iter().map(InventoryResponse::Available);
                    Handler::Finished(Ok(Response::Blocks(available.collect())))
                } else {
                    // Keep on waiting for all the blocks we wanted, until we get them or time out.
                    Handler::BlocksByHash {
                        pending_hashes,
                        blocks,
                    }
                }
            }
            // peers are allowed to return this response, but `zcashd` never does
            (
                Handler::BlocksByHash {
                    pending_hashes,
                    blocks,
                },
                Message::NotFound(missing_invs),
            ) => {
                // assumptions:
                //   - the peer eventually returns a block or a `notfound` entry
                //     for each hash
                //   - all `notfound` entries are contained in a single message
                //   - the `notfound` message comes after the block messages
                //
                // If we're in sync with the peer, then the `notfound` should contain the remaining
                // hashes from the handler. If we're not in sync with the peer, we should return
                // what we got so far, and log an error.
                let missing_blocks: HashSet<_> = block_hashes(&missing_invs).collect();
                if missing_blocks != pending_hashes {
                    trace!(?missing_invs, ?missing_blocks, ?pending_hashes);
                    // if these errors are noisy, we should replace them with debugs
                    info!("unexpected notfound message from peer: all remaining block hashes should be listed in the notfound. Using partial received blocks as the peer response");
                }
                if missing_blocks.len() != missing_invs.len() {
                    trace!(?missing_invs, ?missing_blocks, ?pending_hashes);
                    info!("unexpected notfound message from peer: notfound contains duplicate hashes or non-block hashes. Using partial received blocks as the peer response");
                }

                if blocks.is_empty() {
                    // If we didn't get anything we wanted, retry the request.
                    let missing_block_hashes = pending_hashes.into_iter().map(Into::into).collect();
                    Handler::Finished(Err(PeerError::NotFoundResponse(missing_block_hashes)))
                } else {
                    // If we got some of what we wanted, let the internal client know.
                    let available = blocks.into_iter().map(InventoryResponse::Available);
                    let missing = pending_hashes.into_iter().map(InventoryResponse::Missing);

                    Handler::Finished(Ok(Response::Blocks(available.chain(missing).collect())))
                }
            }

            // TODO:
            // - use `any(inv)` rather than `all(inv)`?
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
        tx: MustUseClientResponseSender,
        span: tracing::Span,
    },
    /// A failure has occurred and we are shutting down the connection.
    Failed,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            State::AwaitingRequest => "AwaitingRequest".to_string(),
            State::AwaitingResponse { handler, .. } => {
                format!("AwaitingResponse({handler})")
            }
            State::Failed => "Failed".to_string(),
        })
    }
}

impl State {
    /// Returns the Zebra internal state as a string.
    pub fn command(&self) -> Cow<'static, str> {
        match self {
            State::AwaitingRequest => "AwaitingRequest".into(),
            State::AwaitingResponse { handler, .. } => {
                format!("AwaitingResponse({})", handler.command()).into()
            }
            State::Failed => "Failed".into(),
        }
    }
}

/// The outcome of mapping an inbound [`Message`] to a [`Request`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "inbound messages must be handled"]
pub enum InboundMessage {
    /// The message was mapped to an inbound [`Request`].
    AsRequest(Request),

    /// The message was consumed by the mapping method.
    ///
    /// For example, it could be cached, treated as an error,
    /// or an internally handled [`Message::Ping`].
    Consumed,

    /// The message was not used by the inbound message handler.
    Unused,
}

impl From<Request> for InboundMessage {
    fn from(request: Request) -> Self {
        InboundMessage::AsRequest(request)
    }
}

/// The channels, services, and associated state for a peer connection.
pub struct Connection<S, Tx> {
    /// The metadata for the connected peer `service`.
    ///
    /// This field is used for debugging.
    #[allow(dead_code)]
    pub connection_info: Arc<ConnectionInfo>,

    /// The state of this connection's current request or response.
    pub(super) state: State,

    /// A timeout for a client request. This is stored separately from
    /// State so that we can move the future out of it independently of
    /// other state handling.
    pub(super) request_timer: Option<Pin<Box<Sleep>>>,

    /// A cached copy of the last unsolicited `addr` or `addrv2` message from this peer.
    ///
    /// When Zebra requests peers, the cache is consumed and returned as a synthetic response.
    /// This works around `zcashd`'s address response rate-limit.
    ///
    /// Multi-peer `addr` or `addrv2` messages replace single-peer messages in the cache.
    /// (`zcashd` also gossips its own address at regular intervals.)
    pub(super) cached_addrs: Vec<MetaAddr>,

    /// The `inbound` service, used to answer requests from this connection's peer.
    pub(super) svc: S,

    /// A channel for requests that Zebra's internal services want to send to remote peers.
    ///
    /// This channel accepts [`Request`]s, and produces [`InProgressClientRequest`]s.
    pub(super) client_rx: ClientRequestReceiver,

    /// A slot for an error shared between the Connection and the Client that uses it.
    ///
    /// `None` unless the connection or client have errored.
    pub(super) error_slot: ErrorSlot,

    /// A channel for sending Zcash messages to the connected peer.
    ///
    /// This channel accepts [`Message`]s.
    ///
    /// The corresponding peer message receiver is passed to [`Connection::run`].
    pub(super) peer_tx: PeerTx<Tx>,

    /// A connection tracker that reduces the open connection count when dropped.
    /// Used to limit the number of open connections in Zebra.
    ///
    /// This field does nothing until it is dropped.
    ///
    /// # Security
    ///
    /// If this connection tracker or `Connection`s are leaked,
    /// the number of active connections will appear higher than it actually is.
    /// If enough connections leak, Zebra will stop making new connections.
    #[allow(dead_code)]
    pub(super) connection_tracker: ConnectionTracker,

    /// The metrics label for this peer. Usually the remote IP and port.
    pub(super) metrics_label: String,

    /// The state for this peer, when the metrics were last updated.
    pub(super) last_metrics_state: Option<Cow<'static, str>>,
}

impl<S, Tx> fmt::Debug for Connection<S, Tx> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // skip the channels, they don't tell us anything useful
        f.debug_struct(std::any::type_name::<Connection<S, Tx>>())
            .field("connection_info", &self.connection_info)
            .field("state", &self.state)
            .field("request_timer", &self.request_timer)
            .field("cached_addrs", &self.cached_addrs.len())
            .field("error_slot", &self.error_slot)
            .field("metrics_label", &self.metrics_label)
            .field("last_metrics_state", &self.last_metrics_state)
            .finish()
    }
}

impl<S, Tx> Connection<S, Tx> {
    /// Return a new connection from its channels, services, and shared state.
    pub(crate) fn new(
        inbound_service: S,
        client_rx: futures::channel::mpsc::Receiver<ClientRequest>,
        error_slot: ErrorSlot,
        peer_tx: Tx,
        connection_tracker: ConnectionTracker,
        connection_info: Arc<ConnectionInfo>,
    ) -> Self {
        let metrics_label = connection_info.connected_addr.get_transient_addr_label();

        Connection {
            connection_info,
            state: State::AwaitingRequest,
            request_timer: None,
            cached_addrs: Vec::new(),
            svc: inbound_service,
            client_rx: client_rx.into(),
            error_slot,
            peer_tx: peer_tx.into(),
            connection_tracker,
            metrics_label,
            last_metrics_state: None,
        }
    }
}

impl<S, Tx> Connection<S, Tx>
where
    S: Service<Request, Response = Response, Error = BoxError>,
    S::Error: Into<BoxError>,
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    /// Consume this `Connection` to form a spawnable future containing its event loop.
    ///
    /// `peer_rx` is a channel for receiving Zcash [`Message`]s from the connected peer.
    /// The corresponding peer message receiver is [`Connection.peer_tx`].
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
        //
        // TODO: turn this comment into a module-level comment, after splitting the module.
        loop {
            self.update_state_metrics(None);

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
                            let unhandled_msg = self.handle_message_as_request(msg).await;

                            if let Some(unhandled_msg) = unhandled_msg {
                                debug!(
                                    %unhandled_msg,
                                    "ignoring unhandled request while awaiting a request"
                                );
                            }
                        }
                        Either::Right((None, _)) => {
                            trace!("client_rx closed, ending connection");

                            // There are no requests to be flushed,
                            // but we need to set an error and update metrics.
                            self.shutdown(PeerError::ClientDropped);
                            break;
                        }
                        Either::Right((Some(req), _)) => {
                            let span = req.span.clone();
                            self.handle_client_request(req).instrument(span).await
                        }
                    }
                }

                // Check whether the handler is finished before waiting for a response message,
                // because the response might be `Nil` or synthetic.
                State::AwaitingResponse {
                    handler: Handler::Finished(_),
                    ref span,
                    ..
                } => {
                    // We have to get rid of the span reference so we can tamper with the state.
                    let span = span.clone();
                    trace!(
                        parent: &span,
                        "returning completed response to client request"
                    );

                    // Replace the state with a temporary value,
                    // so we can take ownership of the response sender.
                    let tmp_state = std::mem::replace(&mut self.state, State::Failed);

                    if let State::AwaitingResponse {
                        handler: Handler::Finished(response),
                        tx,
                        ..
                    } = tmp_state
                    {
                        if let Ok(response) = response.as_ref() {
                            debug!(%response, "finished receiving peer response to Zebra request");
                            // Add a metric for inbound responses to outbound requests.
                            metrics::counter!(
                                "zebra.net.in.responses",
                                1,
                                "command" => response.command(),
                                "addr" => self.metrics_label.clone(),
                            );
                        } else {
                            debug!(error = ?response, "error in peer response to Zebra request");
                        }

                        let _ = tx.send(response.map_err(Into::into));
                    } else {
                        unreachable!("already checked for AwaitingResponse");
                    }

                    self.state = State::AwaitingRequest;
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
                            self.update_state_metrics(format!("Out::Rsp::{}", peer_msg.command()));

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

                            self.update_state_metrics(None);

                            // If the message was not consumed as a response,
                            // check whether it can be handled as a request.
                            let unused_msg = if let Some(request_msg) = request_msg {
                                // do NOT instrument with the request span, this is
                                // independent work
                                self.handle_message_as_request(request_msg).await
                            } else {
                                None
                            };

                            if let Some(unused_msg) = unused_msg {
                                debug!(
                                    %unused_msg,
                                    %self.state,
                                    "ignoring peer message: not a response or a request",
                                );
                            }
                        }
                        Either::Left((Either::Right(_), _peer_fut)) => {
                            trace!(parent: &span, "client request timed out");
                            let e = PeerError::ConnectionReceiveTimeout;

                            // Replace the state with a temporary value,
                            // so we can take ownership of the response sender.
                            self.state = match std::mem::replace(&mut self.state, State::Failed) {
                                // Special case: ping timeouts fail the connection.
                                State::AwaitingResponse {
                                    handler: Handler::Ping(_),
                                    tx,
                                    ..
                                } => {
                                    // We replaced the original state, which means `fail_with` won't see it.
                                    // So we do the state request cleanup manually.
                                    let e = SharedPeerError::from(e);
                                    let _ = tx.send(Err(e.clone()));
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
                            // The client receiver was dropped, so we don't need to send on `tx` here.
                            trace!(parent: &span, "client request was cancelled");
                            self.state = State::AwaitingRequest;
                        }
                    }
                }

                // This connection has failed: stop the event loop, and complete the future.
                State::Failed => break,
            }
        }

        let error = self.error_slot.try_get_error();
        assert!(
            error.is_some(),
            "closing connections must call fail_with() or shutdown() to set the error slot"
        );

        self.update_state_metrics(error.expect("checked is_some").to_string());
    }

    /// Fail this connection.
    ///
    /// If the connection has errored already, re-use the original error.
    /// Otherwise, fail the connection with `error`.
    fn fail_with(&mut self, error: impl Into<SharedPeerError>) {
        let error = error.into();

        debug!(%error,
               client_receiver = ?self.client_rx,
               "failing peer service with error");

        self.shutdown(error);
    }

    /// Handle an internal client request, possibly generating outgoing messages to the
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
            debug!(state = %self.state, %request, "ignoring canceled request");

            metrics::counter!(
                "zebra.net.out.requests.canceled",
                1,
                "command" => request.command(),
                "addr" => self.metrics_label.clone(),
            );
            self.update_state_metrics(format!("Out::Req::Canceled::{}", request.command()));

            return;
        }

        debug!(state = %self.state, %request, "sending request from Zebra to peer");

        // Add a metric for outbound requests.
        metrics::counter!(
            "zebra.net.out.requests",
            1,
            "command" => request.command(),
            "addr" => self.metrics_label.clone(),
        );
        self.update_state_metrics(format!("Out::Req::{}", request.command()));

        let new_handler = match (&self.state, request) {
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

            // Consume the cached addresses from the peer,
            // to work-around a `zcashd` response rate-limit.
            (AwaitingRequest, Peers) if !self.cached_addrs.is_empty() => {
                let cached_addrs = std::mem::take(&mut self.cached_addrs);
                debug!(
                    addrs = cached_addrs.len(),
                    "responding to Peers request using cached addresses",
                );

                Ok(Handler::Finished(Ok(Response::Peers(cached_addrs))))
            }
            (AwaitingRequest, Peers) => self
                .peer_tx
                .send(Message::GetAddr)
                .await
                .map(|()| Handler::Peers),

            (AwaitingRequest, Ping(nonce)) => self
                .peer_tx
                .send(Message::Ping(nonce))
                .await
                .map(|()| Handler::Ping(nonce)),

            (AwaitingRequest, BlocksByHash(hashes)) => {
                self
                    .peer_tx
                    .send(Message::GetData(
                        hashes.iter().map(|h| (*h).into()).collect(),
                    ))
                    .await
                    .map(|()|
                         Handler::BlocksByHash {
                             blocks: Vec::with_capacity(hashes.len()),
                             pending_hashes: hashes,
                         }
                    )
            }
            (AwaitingRequest, TransactionsById(ids)) => {
                self
                    .peer_tx
                    .send(Message::GetData(
                        ids.iter().map(Into::into).collect(),
                    ))
                    .await
                    .map(|()|
                         Handler::TransactionsById {
                             transactions: Vec::with_capacity(ids.len()),
                             pending_ids: ids,
                         })
            }

            (AwaitingRequest, FindBlocks { known_blocks, stop }) => {
                self
                    .peer_tx
                    .send(Message::GetBlocks { known_blocks, stop })
                    .await
                    .map(|()|
                         Handler::FindBlocks
                    )
            }
            (AwaitingRequest, FindHeaders { known_blocks, stop }) => {
                self
                    .peer_tx
                    .send(Message::GetHeaders { known_blocks, stop })
                    .await
                    .map(|()|
                         Handler::FindHeaders
                    )
            }

            (AwaitingRequest, MempoolTransactionIds) => {
                self
                    .peer_tx
                    .send(Message::Mempool)
                    .await
                    .map(|()|
                         Handler::MempoolTransactionIds
                    )
            }

            (AwaitingRequest, PushTransaction(transaction)) => {
                self
                    .peer_tx
                    .send(Message::Tx(transaction))
                    .await
                    .map(|()|
                         Handler::Finished(Ok(Response::Nil))
                    )
            }
            (AwaitingRequest, AdvertiseTransactionIds(hashes)) => {
                self
                    .peer_tx
                    .send(Message::Inv(hashes.iter().map(|h| (*h).into()).collect()))
                    .await
                    .map(|()|
                         Handler::Finished(Ok(Response::Nil))
                    )
            }
            (AwaitingRequest, AdvertiseBlock(hash)) => {
                self
                    .peer_tx
                    .send(Message::Inv(vec![hash.into()]))
                    .await
                    .map(|()|
                         Handler::Finished(Ok(Response::Nil))
                    )
            }
        };

        // Update the connection state with a new handler, or fail with an error.
        match new_handler {
            Ok(handler) => {
                self.state = AwaitingResponse { handler, span, tx };
                self.request_timer = Some(Box::pin(sleep(constants::REQUEST_TIMEOUT)));
            }
            Err(error) => {
                let error = SharedPeerError::from(error);
                let _ = tx.send(Err(error.clone()));
                self.fail_with(error);
            }
        };
    }

    /// Handle `msg` as a request from a peer to this Zebra instance.
    ///
    /// If the message is not handled, it is returned.
    // This function has its own span, because we're creating a new work
    // context (namely, the work of processing the inbound msg as a request)
    #[instrument(name = "msg_as_req", skip(self, msg), fields(msg = msg.command()))]
    async fn handle_message_as_request(&mut self, msg: Message) -> Option<Message> {
        trace!(?msg);
        debug!(state = %self.state, %msg, "received inbound peer message");

        self.update_state_metrics(format!("In::Msg::{}", msg.command()));

        use InboundMessage::*;

        let req = match msg {
            Message::Ping(nonce) => {
                trace!(?nonce, "responding to heartbeat");
                if let Err(e) = self.peer_tx.send(Message::Pong(nonce)).await {
                    self.fail_with(e);
                }
                Consumed
            }
            // These messages shouldn't be sent outside of a handshake.
            Message::Version { .. } => {
                self.fail_with(PeerError::DuplicateHandshake);
                Consumed
            }
            Message::Verack { .. } => {
                self.fail_with(PeerError::DuplicateHandshake);
                Consumed
            }
            // These messages should already be handled as a response if they
            // could be a response, so if we see them here, they were either
            // sent unsolicited, or they were sent in response to a canceled request
            // that we've already forgotten about.
            Message::Reject { .. } => {
                debug!(%msg, "got reject message unsolicited or from canceled request");
                Unused
            }
            Message::NotFound { .. } => {
                debug!(%msg, "got notfound message unsolicited or from canceled request");
                Unused
            }
            Message::Pong(_) => {
                debug!(%msg, "got pong message unsolicited or from canceled request");
                Unused
            }
            Message::Block(_) => {
                debug!(%msg, "got block message unsolicited or from canceled request");
                Unused
            }
            Message::Headers(_) => {
                debug!(%msg, "got headers message unsolicited or from canceled request");
                Unused
            }
            // These messages should never be sent by peers.
            Message::FilterLoad { .. }
            | Message::FilterAdd { .. }
            | Message::FilterClear { .. } => {
                // # Security
                //
                // Zcash connections are not authenticated, so malicious nodes can send fake messages,
                // with connected peers' IP addresses in the IP header.
                //
                // Since we can't verify their source, Zebra needs to ignore unexpected messages,
                // because closing the connection could cause a denial of service or eclipse attack.
                debug!(%msg, "got BIP111 message without advertising NODE_BLOOM");

                // Ignored, but consumed because it is technically a protocol error.
                Consumed
            }
            // Zebra crawls the network proactively, to prevent
            // peers from inserting data into our address book.
            Message::Addr(ref addrs) => {
                // Workaround `zcashd`'s `getaddr` response rate-limit
                if addrs.len() > 1 {
                    // Always refresh the cache with multi-addr messages.
                    debug!(%msg, "caching unsolicited multi-addr message");
                    self.cached_addrs = addrs.clone();
                    Consumed
                } else if addrs.len() == 1 && self.cached_addrs.len() <= 1 {
                    // Only refresh a cached single addr message with another single addr.
                    // (`zcashd` regularly advertises its own address.)
                    debug!(%msg, "caching unsolicited single addr message");
                    self.cached_addrs = addrs.clone();
                    Consumed
                } else {
                    debug!(
                        %msg,
                        "ignoring unsolicited single addr message: already cached a multi-addr message"
                    );
                    Consumed
                }
            }
            Message::Tx(ref transaction) => Request::PushTransaction(transaction.clone()).into(),
            Message::Inv(ref items) => match &items[..] {
                // We don't expect to be advertised multiple blocks at a time,
                // so we ignore any advertisements of multiple blocks.
                [InventoryHash::Block(hash)] => Request::AdvertiseBlock(*hash).into(),

                // Some peers advertise invs with mixed item types.
                // But we're just interested in the transaction invs.
                //
                // TODO: split mixed invs into multiple requests,
                //       but skip runs of multiple blocks.
                tx_ids if tx_ids.iter().any(|item| item.unmined_tx_id().is_some()) => {
                    Request::AdvertiseTransactionIds(transaction_ids(items).collect()).into()
                }

                // Log detailed messages for ignored inv advertisement messages.
                [] => {
                    debug!(%msg, "ignoring empty inv");

                    // This might be a minor protocol error, or it might mean "not found".
                    Unused
                }
                [InventoryHash::Block(_), InventoryHash::Block(_), ..] => {
                    debug!(%msg, "ignoring inv with multiple blocks");
                    Unused
                }
                _ => {
                    debug!(%msg, "ignoring inv with no transactions");
                    Unused
                }
            },
            Message::GetData(ref items) => match &items[..] {
                // Some peers advertise invs with mixed item types.
                // So we suspect they might do the same with getdata.
                //
                // Since we can only handle one message at a time,
                // we treat it as a block request if there are any blocks,
                // or a transaction request if there are any transactions.
                //
                // TODO: split mixed getdata into multiple requests.
                b_hashes
                    if b_hashes
                        .iter()
                        .any(|item| matches!(item, InventoryHash::Block(_))) =>
                {
                    Request::BlocksByHash(block_hashes(items).collect()).into()
                }
                tx_ids if tx_ids.iter().any(|item| item.unmined_tx_id().is_some()) => {
                    Request::TransactionsById(transaction_ids(items).collect()).into()
                }

                // Log detailed messages for ignored getdata request messages.
                [] => {
                    debug!(%msg, "ignoring empty getdata");

                    // This might be a minor protocol error, or it might mean "not found".
                    Unused
                }
                _ => {
                    debug!(%msg, "ignoring getdata with no blocks or transactions");
                    Unused
                }
            },
            Message::GetAddr => Request::Peers.into(),
            Message::GetBlocks {
                ref known_blocks,
                stop,
            } => Request::FindBlocks {
                known_blocks: known_blocks.clone(),
                stop,
            }
            .into(),
            Message::GetHeaders {
                ref known_blocks,
                stop,
            } => Request::FindHeaders {
                known_blocks: known_blocks.clone(),
                stop,
            }
            .into(),
            Message::Mempool => Request::MempoolTransactionIds.into(),
        };

        // Handle the request, and return unused messages.
        match req {
            AsRequest(req) => {
                self.drive_peer_request(req).await;
                None
            }
            Consumed => None,
            Unused => Some(msg),
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

        // Add a metric for inbound requests
        metrics::counter!(
            "zebra.net.in.requests",
            1,
            "command" => req.command(),
            "addr" => self.metrics_label.clone(),
        );
        self.update_state_metrics(format!("In::Req::{}", req.command()));

        if self.svc.ready().await.is_err() {
            // Treat all service readiness errors as Overloaded
            // TODO: treat `TryRecvError::Closed` in `Inbound::poll_ready` as a fatal error (#1655)
            self.fail_with(PeerError::Overloaded);
            return;
        }

        let rsp = match self.svc.call(req.clone()).await {
            Err(e) => {
                if e.is::<Overloaded>() {
                    tracing::info!("inbound service is overloaded, closing connection");
                    metrics::counter!("pool.closed.loadshed", 1);
                    self.fail_with(PeerError::Overloaded);
                } else {
                    // We could send a reject to the remote peer, but that might cause
                    // them to disconnect, and we might be using them to sync blocks.
                    // For similar reasons, we don't want to fail_with() here - we
                    // only close the connection if the peer is doing something wrong.
                    info!(%e,
                          connection_state = ?self.state,
                          client_receiver = ?self.client_rx,
                          "error processing peer request");
                }
                return;
            }
            Ok(rsp) => rsp,
        };

        // Add a metric for outbound responses to inbound requests
        metrics::counter!(
            "zebra.net.out.responses",
            1,
            "command" => rsp.command(),
            "addr" => self.metrics_label.clone(),
        );
        self.update_state_metrics(format!("In::Rsp::{}", rsp.command()));

        match rsp.clone() {
            Response::Nil => { /* generic success, do nothing */ }
            Response::Peers(addrs) => {
                if let Err(e) = self.peer_tx.send(Message::Addr(addrs)).await {
                    self.fail_with(e);
                }
            }
            Response::Transactions(transactions) => {
                // Generate one tx message per transaction,
                // then a notfound message with all the missing transaction ids.
                let mut missing_ids = Vec::new();

                for transaction in transactions.into_iter() {
                    match transaction {
                        Available(transaction) => {
                            if let Err(e) = self.peer_tx.send(Message::Tx(transaction)).await {
                                self.fail_with(e);
                                return;
                            }
                        }
                        Missing(id) => missing_ids.push(id.into()),
                    }
                }

                if !missing_ids.is_empty() {
                    if let Err(e) = self.peer_tx.send(Message::NotFound(missing_ids)).await {
                        self.fail_with(e);
                        return;
                    }
                }
            }
            Response::Blocks(blocks) => {
                // Generate one tx message per block,
                // then a notfound message with all the missing block hashes.
                let mut missing_hashes = Vec::new();

                for block in blocks.into_iter() {
                    match block {
                        Available(block) => {
                            if let Err(e) = self.peer_tx.send(Message::Block(block)).await {
                                self.fail_with(e);
                                return;
                            }
                        }
                        Missing(hash) => missing_hashes.push(hash.into()),
                    }
                }

                if !missing_hashes.is_empty() {
                    if let Err(e) = self.peer_tx.send(Message::NotFound(missing_hashes)).await {
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

        debug!(state = %self.state, %req, %rsp, "sent Zebra response to peer");
    }
}

impl<S, Tx> Connection<S, Tx> {
    /// Update the connection state metrics for this connection,
    /// using `extra_state_info` as additional state information.
    fn update_state_metrics(&mut self, extra_state_info: impl Into<Option<String>>) {
        let current_metrics_state = if let Some(extra_state_info) = extra_state_info.into() {
            format!("{}::{extra_state_info}", self.state.command()).into()
        } else {
            self.state.command()
        };

        if self.last_metrics_state.as_ref() == Some(&current_metrics_state) {
            return;
        }

        self.erase_state_metrics();

        // Set the new state
        metrics::increment_gauge!(
            "zebra.net.connection.state",
            1.0,
            "command" => current_metrics_state.clone(),
            "addr" => self.metrics_label.clone(),
        );

        self.last_metrics_state = Some(current_metrics_state);
    }

    /// Erase the connection state metrics for this connection.
    fn erase_state_metrics(&mut self) {
        if let Some(last_metrics_state) = self.last_metrics_state.take() {
            metrics::gauge!(
                "zebra.net.connection.state",
                0.0,
                "command" => last_metrics_state,
                "addr" => self.metrics_label.clone(),
            );
        }
    }

    /// Marks the peer as having failed with `error`, and performs connection cleanup.
    ///
    /// If the connection has errored already, re-use the original error.
    /// Otherwise, fail the connection with `error`.
    fn shutdown(&mut self, error: impl Into<SharedPeerError>) {
        let mut error = error.into();

        // Close channels first, so other tasks can start shutting down.
        //
        // TODO: close peer_tx and peer_rx, after:
        // - adapting them using a struct with a Stream impl, rather than closures
        // - making the struct forward `close` to the inner channel
        self.client_rx.close();

        // Update the shared error slot
        //
        // # Correctness
        //
        // Error slots use a threaded `std::sync::Mutex`, so accessing the slot
        // can block the async task's current thread. We only perform a single
        // slot update per `Client`. We ignore subsequent error slot updates.
        let slot_result = self.error_slot.try_update_error(error.clone());

        if let Err(AlreadyErrored { original_error }) = slot_result {
            debug!(
                new_error = %error,
                %original_error,
                connection_state = ?self.state,
                "multiple errors on connection: \
                 failed connections should stop processing pending requests and responses, \
                 then close the connection"
            );

            error = original_error;
        } else {
            debug!(%error,
                   connection_state = ?self.state,
                   "shutting down peer service with error");
        }

        // Prepare to flush any pending client requests.
        //
        // We've already closed the client channel, so setting State::Failed
        // will make the main loop flush any pending requests.
        //
        // However, we may have an outstanding client request in State::AwaitingResponse,
        // so we need to deal with it first.
        if let State::AwaitingResponse { tx, .. } =
            std::mem::replace(&mut self.state, State::Failed)
        {
            // # Correctness
            //
            // We know the slot has Some(error), because we just set it above,
            // and the error slot is never unset.
            //
            // Accessing the error slot locks a threaded std::sync::Mutex, which
            // can block the current async task thread. We briefly lock the mutex
            // to clone the error.
            let _ = tx.send(Err(error.clone()));
        }

        // Make the timer and metrics consistent with the Failed state.
        self.request_timer = None;
        self.update_state_metrics(None);

        // Finally, flush pending client requests.
        while let Some(InProgressClientRequest { tx, span, .. }) =
            self.client_rx.close_and_flush_next()
        {
            trace!(
                parent: &span,
                %error,
                "sending an error response to a pending request on a failed connection"
            );
            let _ = tx.send(Err(error.clone()));
        }
    }
}

impl<S, Tx> Drop for Connection<S, Tx> {
    fn drop(&mut self) {
        self.shutdown(PeerError::ConnectionDropped);

        self.erase_state_metrics();
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
    items.iter().filter_map(InventoryHash::block_hash)
}
