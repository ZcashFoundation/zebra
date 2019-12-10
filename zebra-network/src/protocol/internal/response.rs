use std::error::Error;

#[cfg(test)]
use proptest_derive::Arbitrary;

// XXX clean module layout of zebra_chain
use zebra_chain::transaction::Transaction;

use crate::meta_addr::MetaAddr;

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
    /// A list of transactions, such as in response to `GetMempool
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

    use super::Response;

    proptest! {
        #[test]
        fn proptest_response(res in any::<Response>()) {
            println!("{:?}", res);
        }
    }
}
