//! Message types for the internal request/response protocol.
//!
//! These are currently defined just as enums with all possible requests and
//! responses, so that we have unified types to pass around. No serialization
//! is performed as these are only internal types.

use std::error::Error;

#[cfg(test)]
use proptest_derive::Arbitrary;

use zebra_chain::transaction::Transaction;

use crate::meta_addr::MetaAddr;

use super::types::Nonce;

/// A network request, represented in internal format.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
pub enum Request {
    /// Requests additional peers from the server.
    GetPeers,
    /// Advertises peers to the remote server.
    #[cfg_attr(test, proptest(skip))]
    PushPeers(Vec<MetaAddr>),
    /// Heartbeats triggered on peer connection start.
    // This is included as a bit of a hack, it should only be used
    // internally for connection management. You should not expect to
    // be firing or handling `Ping` requests or `Pong` responses.
    #[cfg_attr(test, proptest(skip))]
    Ping(Nonce),
    /// Requests the transactions the remote server has verified but
    /// not yet confirmed.
    GetMempool,
}

/// A response to a network request, represented in internal format.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
pub enum Response {
    /// Generic success.
    Ok,
    /// Generic error.
    Error,
    /// A list of peers, used to respond to `GetPeers`.
    #[cfg_attr(test, proptest(skip))]
    Peers(Vec<MetaAddr>),
    /// A list of transactions, such as in response to `GetMempool`.
    #[cfg_attr(test, proptest(skip))]
    Transactions(Vec<Transaction>),
}

impl<E> From<E> for Response
where
    E: Error,
{
    fn from(_e: E) -> Self {
        Self::Error
    }
}

#[cfg(test)]
mod test {
    use proptest::prelude::*;

    use super::{Request, Response};

    proptest! {

        #[test]
        fn proptest_request(req in any::<Request>()) {
            println!("{:?}", req);
        }

        #[test]
        fn proptest_response(res in any::<Response>()) {
            println!("{:?}", res);
        }
    }
}
