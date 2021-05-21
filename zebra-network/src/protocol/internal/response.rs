use zebra_chain::{
    block::{self, Block},
    transaction::{self, Transaction},
};

use crate::meta_addr::MetaAddr;

use std::{fmt, sync::Arc};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A response to a network request, represented in internal format.
#[derive(Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Response {
    /// Do not send any response to this request.
    ///
    /// Either:
    ///  * the request does not need a response, or
    ///  * we have no useful data to provide in response to the request.
    Nil,

    /// A list of peers, used to respond to `GetPeers`.
    Peers(Vec<MetaAddr>),

    /// A list of blocks.
    Blocks(Vec<Arc<Block>>),

    /// A list of block hashes.
    BlockHashes(Vec<block::Hash>),

    /// A list of block headers.
    BlockHeaders(Vec<block::CountedHeader>),

    /// A list of transactions.
    Transactions(Vec<Arc<Transaction>>),

    /// A list of transaction hashes.
    TransactionHashes(Vec<transaction::Hash>),
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Response::Nil => "Nil",
            // TODO: add length
            Response::Peers(_) => "Peers",
            Response::Blocks(_) => "Blocks",
            Response::BlockHashes(_) => "BlockHashes",
            Response::BlockHeaders(_) => "BlockHeaders",
            Response::Transactions(_) => "Transactions",
            Response::TransactionHashes(_) => "TransactionHashes",
        })
    }
}
