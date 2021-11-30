use zebra_chain::{
    block::{self, Block},
    transaction::{UnminedTx, UnminedTxId},
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
    ///  * we have no useful data to provide in response to the request
    ///
    /// When Zebra doesn't have any useful data, it always sends no response,
    /// instead of sending `notfound`. `zcashd` sometimes sends no response,
    /// and sometimes sends `notfound`.
    Nil,

    /// A list of peers, used to respond to `GetPeers`.
    ///
    /// The list contains `0..=MAX_META_ADDR` peers.
    Peers(Vec<MetaAddr>),

    /// A list of blocks.
    ///
    /// The list contains zero or more blocks.
    Blocks(Vec<Arc<Block>>),

    /// A list of block hashes.
    ///
    /// The list contains zero or more block hashes.
    BlockHashes(Vec<block::Hash>),

    /// A list of block headers.
    ///
    /// The list contains zero or more block headers.
    BlockHeaders(Vec<block::CountedHeader>),

    /// A list of unmined transactions.
    ///
    /// The list contains zero or more unmined transactions.
    Transactions(Vec<UnminedTx>),

    /// A list of unmined transaction IDs.
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// The list contains zero or more transaction IDs.
    TransactionIds(Vec<UnminedTxId>),
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Response::Nil => "Nil".to_string(),

            Response::Peers(peers) => format!("Peers {{ peers: {} }}", peers.len()),

            Response::Blocks(blocks) => format!("Blocks {{ blocks: {} }}", blocks.len()),
            Response::BlockHashes(hashes) => format!("BlockHashes {{ hashes: {} }}", hashes.len()),
            Response::BlockHeaders(headers) => {
                format!("BlockHeaders {{ headers: {} }}", headers.len())
            }

            Response::Transactions(transactions) => {
                format!("Transactions {{ transactions: {} }}", transactions.len())
            }
            Response::TransactionIds(ids) => format!("TransactionIds {{ ids: {} }}", ids.len()),
        })
    }
}
