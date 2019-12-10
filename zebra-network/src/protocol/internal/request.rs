#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::meta_addr::MetaAddr;

use super::super::types::Nonce;

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

#[cfg(test)]
mod test {
    use proptest::prelude::*;

    use super::Request;

    proptest! {
        #[test]
        fn proptest_response(res in any::<Request>()) {
            println!("{:?}", res);
        }
    }
}
