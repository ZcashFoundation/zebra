use zebra_chain::{
    block::{self, Block},
    transaction::{UnminedTx, UnminedTxId},
};

use crate::meta_addr::MetaAddr;

use std::{fmt, sync::Arc};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A response to a network request, represented in internal format.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    //
    // TODO: make this into a HashMap<SocketAddr, MetaAddr> - a unique list of peer addresses (#2244)
    Peers(Vec<MetaAddr>),

    /// A list of blocks.
    ///
    /// The list contains zero or more blocks.
    //
    // TODO: split this into found and not found (#2726)
    Blocks(Vec<Arc<Block>>),

    /// A list of block hashes.
    ///
    /// The list contains zero or more block hashes.
    //
    // TODO: make this into an IndexMap - an ordered unique list of hashes (#2244)
    BlockHashes(Vec<block::Hash>),

    /// A list of block headers.
    ///
    /// The list contains zero or more block headers.
    //
    // TODO: make this into a HashMap<block::Hash, CountedHeader> - a unique list of headers (#2244)
    //       split this into found and not found (#2726)
    BlockHeaders(Vec<block::CountedHeader>),

    /// A list of unmined transactions.
    ///
    /// The list contains zero or more unmined transactions.
    //
    // TODO: split this into found and not found (#2726)
    Transactions(Vec<UnminedTx>),

    /// A list of unmined transaction IDs.
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// The list contains zero or more transaction IDs.
    //
    // TODO: make this into a HashSet - a unique list of transaction IDs (#2244)
    TransactionIds(Vec<UnminedTxId>),
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Response::Nil => "Nil".to_string(),

            Response::Peers(peers) => format!("Peers {{ peers: {} }}", peers.len()),

            // Display heights for single-block responses (which Zebra requests and expects)
            Response::Blocks(blocks) if blocks.len() == 1 => {
                let block = blocks.first().expect("len is 1");
                format!(
                    "Block {{ height: {}, hash: {} }}",
                    block
                        .coinbase_height()
                        .as_ref()
                        .map(|h| h.0.to_string())
                        .unwrap_or_else(|| "None".into()),
                    block.hash(),
                )
            }
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

impl Response {
    /// Returns the Zebra internal response type as a string.
    pub fn command(&self) -> &'static str {
        match self {
            Response::Nil => "Nil",

            Response::Peers(_) => "Peers",

            Response::Blocks(_) => "Blocks",
            Response::BlockHashes(_) => "BlockHashes",

            Response::BlockHeaders { .. } => "BlockHeaders",

            Response::Transactions(_) => "Transactions",
            Response::TransactionIds(_) => "TransactionIds",
        }
    }
}
