//! Zebra's internal peer message response format.

use std::{fmt, sync::Arc, time::Duration};

use zebra_chain::{
    block::{self, Block},
    transaction::{UnminedTx, UnminedTxId},
};

use crate::{meta_addr::MetaAddr, protocol::internal::InventoryResponse, PeerSocketAddr};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use InventoryResponse::*;

/// A response to a network request, represented in internal format.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Response {
    /// The request does not have a response.
    ///
    /// Either:
    ///  * the request does not need a response, or
    ///  * we have no useful data to provide in response to the request,
    ///    and the request was not an inventory request.
    ///
    /// (Inventory requests provide a list of missing hashes if none of the hashes were available.)
    Nil,

    /// A list of peers, used to respond to `GetPeers`.
    ///
    /// The list contains `0..=MAX_META_ADDR` peers.
    //
    // TODO: make this into a HashMap<PeerSocketAddr, MetaAddr> - a unique list of peer addresses (#2244)
    Peers(Vec<MetaAddr>),

    /// A pong response containing the round-trip latency for a peer.
    ///
    /// Returned after a ping/pong exchange to measure RTT.
    Pong(Duration),

    /// An ordered list of block hashes.
    ///
    /// The list contains zero or more block hashes.
    //
    // TODO: make this into an IndexMap - an ordered unique list of hashes (#2244)
    BlockHashes(Vec<block::Hash>),

    /// An ordered list of block headers.
    ///
    /// The list contains zero or more block headers.
    //
    // TODO: make this into an IndexMap - an ordered unique list of headers (#2244)
    BlockHeaders(Vec<block::CountedHeader>),

    /// A list of unmined transaction IDs.
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// The list contains zero or more transaction IDs.
    //
    // TODO: make this into a HashSet - a unique list (#2244)
    TransactionIds(Vec<UnminedTxId>),

    /// A list of found blocks, and missing block hashes.
    ///
    /// Each list contains zero or more entries.
    ///
    /// When Zebra doesn't have a block or transaction, it always sends `notfound`.
    /// `zcashd` sometimes sends no response, and sometimes sends `notfound`.
    //
    // TODO: make this into a HashMap<block::Hash, InventoryResponse<Arc<Block>, ()>> - a unique list (#2244)
    Blocks(Vec<InventoryResponse<(Arc<Block>, Option<PeerSocketAddr>), block::Hash>>),

    /// A list of found unmined transactions, and missing unmined transaction IDs.
    ///
    /// Each list contains zero or more entries.
    //
    // TODO: make this into a HashMap<UnminedTxId, InventoryResponse<UnminedTx, ()>> - a unique list (#2244)
    Transactions(Vec<InventoryResponse<(UnminedTx, Option<PeerSocketAddr>), UnminedTxId>>),
}

impl fmt::Display for Response {
    #[allow(clippy::unwrap_in_result)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Response::Nil => "Nil".to_string(),

            Response::Peers(peers) => format!("Peers {{ peers: {} }}", peers.len()),

            Response::Pong(duration) => format!("Pong {{ latency: {:?} }}", duration),

            Response::BlockHashes(hashes) => format!("BlockHashes {{ hashes: {} }}", hashes.len()),
            Response::BlockHeaders(headers) => {
                format!("BlockHeaders {{ headers: {} }}", headers.len())
            }
            Response::TransactionIds(ids) => format!("TransactionIds {{ ids: {} }}", ids.len()),

            // Display heights for single-block responses (which Zebra requests and expects)
            Response::Blocks(blocks) if blocks.len() == 1 => {
                match blocks.first().expect("len is 1") {
                    Available((block, _)) => format!(
                        "Block {{ height: {}, hash: {} }}",
                        block
                            .coinbase_height()
                            .as_ref()
                            .map(|h| h.0.to_string())
                            .unwrap_or_else(|| "None".into()),
                        block.hash(),
                    ),
                    Missing(hash) => format!("Block {{ missing: {hash} }}"),
                }
            }
            Response::Blocks(blocks) => format!(
                "Blocks {{ blocks: {}, missing: {} }}",
                blocks.iter().filter(|r| r.is_available()).count(),
                blocks.iter().filter(|r| r.is_missing()).count()
            ),

            Response::Transactions(transactions) => format!(
                "Transactions {{ transactions: {}, missing: {} }}",
                transactions.iter().filter(|r| r.is_available()).count(),
                transactions.iter().filter(|r| r.is_missing()).count()
            ),
        })
    }
}

impl Response {
    /// Returns the Zebra internal response type as a string.
    pub fn command(&self) -> &'static str {
        match self {
            Response::Nil => "Nil",

            Response::Peers(_) => "Peers",

            Response::Pong(_) => "Pong",

            Response::BlockHashes(_) => "BlockHashes",
            Response::BlockHeaders(_) => "BlockHeaders",
            Response::TransactionIds(_) => "TransactionIds",

            Response::Blocks(_) => "Blocks",
            Response::Transactions(_) => "Transactions",
        }
    }

    /// Returns true if the response is a block or transaction inventory download.
    pub fn is_inventory_download(&self) -> bool {
        matches!(self, Response::Blocks(_) | Response::Transactions(_))
    }

    /// Returns true if self is the [`Response::Nil`] variant.
    #[allow(dead_code)]
    pub fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }
}
