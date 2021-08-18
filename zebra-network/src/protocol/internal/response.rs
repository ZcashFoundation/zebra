use zebra_chain::{
    block::{self, Block},
    transaction::{UnminedTx, UnminedTxId},
};

use crate::meta_addr::MetaAddr;

use std::sync::Arc;

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
    ///  * we have no useful data to provide in response to the request
    ///
    /// When Zebra doesn't have any useful data, it always sends no response,
    /// instead of sending `notfound`. `zcashd` sometimes sends no response,
    /// and sometimes sends `notfound`.
    Nil,

    /// A list of peers, used to respond to `GetPeers`.
    Peers(Vec<MetaAddr>),

    /// A list of blocks.
    Blocks(Vec<Arc<Block>>),

    /// A list of block hashes.
    BlockHashes(Vec<block::Hash>),

    /// A list of block headers.
    BlockHeaders(Vec<block::CountedHeader>),

    /// A list of unmined transactions.
    Transactions(Vec<UnminedTx>),

    /// A list of unmined transaction IDs.
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    TransactionIds(Vec<UnminedTxId>),
}
