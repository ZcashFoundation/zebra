//! Message types for the internal request/response protocol.
//!
//! These are currently defined just as enums with all possible requests and
//! responses, so that we have unified types to pass around. No serialization
//! is performed as these are only internal types.

use zebra_chain::transaction::Transaction;

use crate::meta_addr::MetaAddr;

use super::types::Nonce;

/// A network request, represented in internal format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// Requests additional peers from the server.
    GetPeers,
    /// Advertises peers to the remote server.
    PushPeers(Vec<MetaAddr>),
    /// Heartbeats triggered on peer connection start.
    // This is included as a bit of a hack, it should only be used
    // internally for connection management. You should not expect to
    // be firing or handling `Ping` requests or `Pong` responses.
    Ping(Nonce),
    /// Requests the transactions the remote server has verified but
    /// not yet confirmed.
    GetMempool,
}

/// A response to a network request, represented in internal format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Response {
    /// Generic success.
    Ok,
    /// A list of peers, used to respond to `GetPeers`.
    Peers(Vec<MetaAddr>),
    /// A list of transactions, such as in response to `GetMempool`.
    Transactions(Vec<Transaction>),
}
