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

/// Revocable permission to serve a particular immutable block snapshot.
///
/// Check immediately before starting the local write, after waiting for sink readiness.
#[derive(Clone)]
pub struct BlockAuthorization(Arc<dyn Fn(&Arc<Block>) -> bool + Send + Sync>);

impl BlockAuthorization {
    /// Creates a live authorization check without transferring validation authority to the network.
    pub fn new(check: impl Fn(&Arc<Block>) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }

    /// Returns whether this exact body may still be sent.
    pub fn is_authorized(&self, block: &Arc<Block>) -> bool {
        (self.0)(block)
    }
}

impl fmt::Debug for BlockAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockAuthorization").finish_non_exhaustive()
    }
}

impl PartialEq for BlockAuthorization {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for BlockAuthorization {}

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

    /// Blocks downloaded from a connection that bilaterally negotiated experimental early relay.
    ///
    /// All block responses on such connections carry this provenance, even for committed blocks.
    /// The source address is unchanged. This is not evidence of validity: receivers authenticate
    /// the header and body before exempting later validation failures from punishment.
    /// Inbound handlers return [`Self::Blocks`] or [`Self::AuthorizedBlocks`], not this variant.
    EarlyBlocks(Vec<InventoryResponse<(Arc<Block>, Option<PeerSocketAddr>), block::Hash>>),

    /// Inbound block responses containing uncommitted bodies with revocable serving permission.
    ///
    /// Unlike [`Self::EarlyBlocks`], this is never returned by an outbound download.
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    AuthorizedBlocks {
        /// Found bodies and missing hashes, as in [`Self::Blocks`].
        blocks: Vec<InventoryResponse<(Arc<Block>, Option<PeerSocketAddr>), block::Hash>>,
        /// Rechecked separately for each body immediately before its local write begins.
        authorization: BlockAuthorization,
    },

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

            Response::Pong(duration) => format!("Pong {{ latency: {duration:?} }}"),

            Response::BlockHashes(hashes) => format!("BlockHashes {{ hashes: {} }}", hashes.len()),
            Response::BlockHeaders(headers) => {
                format!("BlockHeaders {{ headers: {} }}", headers.len())
            }
            Response::TransactionIds(ids) => format!("TransactionIds {{ ids: {} }}", ids.len()),

            // Display heights for single-block responses (which Zebra requests and expects)
            Response::Blocks(blocks)
            | Response::EarlyBlocks(blocks)
            | Response::AuthorizedBlocks { blocks, .. }
                if blocks.len() == 1 =>
            {
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
            Response::Blocks(blocks)
            | Response::EarlyBlocks(blocks)
            | Response::AuthorizedBlocks { blocks, .. } => format!(
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
            Response::EarlyBlocks(_) => "EarlyBlocks",
            Response::AuthorizedBlocks { .. } => "AuthorizedBlocks",
            Response::Transactions(_) => "Transactions",
        }
    }

    /// Returns true if the response is a block or transaction inventory download.
    pub fn is_inventory_download(&self) -> bool {
        matches!(
            self,
            Response::Blocks(_) | Response::EarlyBlocks(_) | Response::Transactions(_)
        )
    }

    /// Returns true if self is the [`Response::Nil`] variant.
    #[allow(dead_code)]
    pub fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }
}
