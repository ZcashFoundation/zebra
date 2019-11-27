use crate::meta_addr::MetaAddr;

use super::super::types::Nonce;

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
