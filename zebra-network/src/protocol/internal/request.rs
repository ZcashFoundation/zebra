use std::{collections::HashSet, fmt};

use zebra_chain::{
    block,
    transaction::{UnminedTx, UnminedTxId},
};

use super::super::types::Nonce;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A network request, represented in internal format.
///
/// The network layer aims to abstract away the details of the Bitcoin wire
/// protocol into a clear request/response API. Each [`Request`] documents the
/// possible [`Response`s](super::Response) it can generate; it is fine (and
/// recommended!) to match on the expected responses and treat the others as
/// `unreachable!()`, since their return indicates a bug in the network code.
///
/// # Cancellations
///
/// The peer set handles cancelled requests (i.e., requests where the future
/// returned by `Service::call` is dropped before it resolves) on a best-effort
/// basis. Requests are routed to a particular peer connection, and then
/// translated into Zcash protocol messages and sent over the network. If a
/// request is cancelled after it is submitted but before it is processed by a
/// peer connection, no messages will be sent. Otherwise, if it is cancelled
/// while waiting for a response, the peer connection resets its state and makes
/// a best-effort attempt to ignore any messages responsive to the cancelled
/// request, subject to limitations in the underlying Zcash protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Request {
    /// Requests additional peers from the server.
    ///
    /// # Response
    ///
    /// Returns [`Response::Peers`](super::Response::Peers).
    Peers,

    /// Heartbeats triggered on peer connection start.
    ///
    /// This is included as a bit of a hack, it should only be used
    /// internally for connection management. You should not expect to
    /// be firing or handling `Ping` requests or `Pong` responses.
    #[doc(hidden)]
    Ping(Nonce),

    /// Request block data by block hashes.
    ///
    /// This uses a `HashSet` rather than a `Vec` for two reasons. First, it
    /// automatically deduplicates the requested blocks. Second, the internal
    /// protocol translator needs to maintain a `HashSet` anyways, in order to
    /// keep track of which requested blocks have been received and when the
    /// request is ready. Rather than force the internals to always convert into
    /// a `HashSet`, we require the caller to pass one, so that if the caller
    /// didn't start with a `Vec` but with, e.g., an iterator, they can collect
    /// directly into a `HashSet` and save work.
    ///
    /// If this requests a recently-advertised block, the peer set will make a
    /// best-effort attempt to route the request to a peer that advertised the
    /// block. This routing is only used for request sets of size 1.
    /// Otherwise, it is routed using the normal load-balancing strategy.
    ///
    /// The list contains zero or more block hashes.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Blocks`](super::Response::Blocks).
    BlocksByHash(HashSet<block::Hash>),

    /// Request transactions by their unmined transaction ID.
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// This uses a `HashSet` for the same reason as [`Request::BlocksByHash`].
    ///
    /// If this requests a recently-advertised transaction, the peer set will
    /// make a best-effort attempt to route the request to a peer that advertised
    /// the transaction. This routing is only used for request sets of size 1.
    /// Otherwise, it is routed using the normal load-balancing strategy.
    ///
    /// The list contains zero or more unmined transaction IDs.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Transactions`](super::Response::Transactions).
    TransactionsById(HashSet<UnminedTxId>),

    /// Request block hashes of subsequent blocks in the chain, given hashes of
    /// known blocks.
    ///
    /// The known blocks list contains zero or more block hashes.
    ///
    /// # Returns
    ///
    /// Returns
    /// [`Response::BlockHashes`](super::Response::BlockHashes).
    ///
    /// # Warning
    ///
    /// This is implemented by sending a `getblocks` message. Bitcoin nodes
    /// respond to `getblocks` with an `inv` message containing a list of the
    /// subsequent blocks. However, Bitcoin nodes *also* send `inv` messages
    /// unsolicited in order to gossip new blocks to their peers. These gossip
    /// messages can race with the response to a `getblocks` request, and there
    /// is no way for the network layer to distinguish them. For this reason, the
    /// response may occasionally contain a single hash of a new chain tip rather
    /// than a list of hashes of subsequent blocks. We believe that unsolicited
    /// `inv` messages will always have exactly one block hash.
    FindBlocks {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        //
        // TODO: make this into an IndexMap - an ordered unique list of hashes (#2244)
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last block hash to request.
        stop: Option<block::Hash>,
    },

    /// Request headers of subsequent blocks in the chain, given hashes of
    /// known blocks.
    ///
    /// The known blocks list contains zero or more block hashes.
    ///
    /// # Returns
    ///
    /// Returns
    /// [`Response::BlockHeaders`](super::Response::BlockHeaders).
    FindHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        //
        // TODO: make this into an IndexMap - an ordered unique list of hashes (#2244)
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last header to request.
        stop: Option<block::Hash>,
    },

    /// Push an unmined transaction to a remote peer, without advertising it to them first.
    ///
    /// This is implemented by sending an unsolicited `tx` message.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    PushTransaction(UnminedTx),

    /// Advertise a set of unmined transactions to all peers.
    ///
    /// Both Zebra and zcashd sometimes advertise multiple transactions at once.
    ///
    /// This is implemented by sending an `inv` message containing the unmined
    /// transaction IDs, allowing the remote peer to choose whether to download
    /// them. Remote peers who choose to download the transaction will generate a
    /// [`Request::TransactionsById`] against the "inbound" service passed to
    /// [`init`](crate::init).
    ///
    /// v4 transactions use a legacy transaction ID, and
    /// v5 transactions use a witnessed transaction ID.
    ///
    /// The list contains zero or more transaction IDs.
    ///
    /// The peer set routes this request specially, sending it to *half of*
    /// the available peers.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    AdvertiseTransactionIds(HashSet<UnminedTxId>),

    /// Advertise a block to all peers.
    ///
    /// This is implemented by sending an `inv` message containing the
    /// block hash, allowing the remote peer to choose whether to download
    /// it. Remote peers who choose to download the block will generate a
    /// [`Request::BlocksByHash`] against the "inbound" service passed to
    /// [`init`](crate::init).
    ///
    /// The peer set routes this request specially, sending it to *a fraction of*
    /// the available peers. See [`number_of_peers_to_broadcast()`](crate::PeerSet::number_of_peers_to_broadcast)
    /// for more details.
    ///
    /// # Returns
    ///
    /// Returns [`Response::Nil`](super::Response::Nil).
    AdvertiseBlock(block::Hash),

    /// Advertise a block to all ready peers. This is equivalent to
    /// [`Request::AdvertiseBlock`] except that the peer set will route
    /// this request to all available ready peers. Used by the gossip task
    /// to broadcast mined blocks to all ready peers.
    AdvertiseBlockToAll(block::Hash),

    /// Request the contents of this node's mempool.
    ///
    /// # Returns
    ///
    /// Returns [`Response::TransactionIds`](super::Response::TransactionIds).
    MempoolTransactionIds,
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&match self {
            Request::Peers => "Peers".to_string(),
            Request::Ping(_) => "Ping".to_string(),

            Request::BlocksByHash(hashes) => {
                format!("BlocksByHash({})", hashes.len())
            }
            Request::TransactionsById(ids) => format!("TransactionsById({})", ids.len()),

            Request::FindBlocks { known_blocks, stop } => format!(
                "FindBlocks {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                if stop.is_some() { "Some" } else { "None" },
            ),
            Request::FindHeaders { known_blocks, stop } => format!(
                "FindHeaders {{ known_blocks: {}, stop: {} }}",
                known_blocks.len(),
                if stop.is_some() { "Some" } else { "None" },
            ),

            Request::PushTransaction(_) => "PushTransaction".to_string(),
            Request::AdvertiseTransactionIds(ids) => {
                format!("AdvertiseTransactionIds({})", ids.len())
            }

            Request::AdvertiseBlock(_) => "AdvertiseBlock".to_string(),
            Request::AdvertiseBlockToAll(_) => "AdvertiseBlockToAll".to_string(),
            Request::MempoolTransactionIds => "MempoolTransactionIds".to_string(),
        })
    }
}

impl Request {
    /// Returns the Zebra internal request type as a string.
    pub fn command(&self) -> &'static str {
        match self {
            Request::Peers => "Peers",
            Request::Ping(_) => "Ping",

            Request::BlocksByHash(_) => "BlocksByHash",
            Request::TransactionsById(_) => "TransactionsById",

            Request::FindBlocks { .. } => "FindBlocks",
            Request::FindHeaders { .. } => "FindHeaders",

            Request::PushTransaction(_) => "PushTransaction",
            Request::AdvertiseTransactionIds(_) => "AdvertiseTransactionIds",

            Request::AdvertiseBlock(_) | Request::AdvertiseBlockToAll(_) => "AdvertiseBlock",
            Request::MempoolTransactionIds => "MempoolTransactionIds",
        }
    }

    /// Returns true if the request is for block or transaction inventory downloads.
    pub fn is_inventory_download(&self) -> bool {
        matches!(
            self,
            Request::BlocksByHash(_) | Request::TransactionsById(_)
        )
    }

    /// Returns the block hash inventory downloads from the request, if any.
    pub fn block_hash_inventory(&self) -> HashSet<block::Hash> {
        if let Request::BlocksByHash(block_hashes) = self {
            block_hashes.clone()
        } else {
            HashSet::new()
        }
    }

    /// Returns the transaction ID inventory downloads from the request, if any.
    pub fn transaction_id_inventory(&self) -> HashSet<UnminedTxId> {
        if let Request::TransactionsById(transaction_ids) = self {
            transaction_ids.clone()
        } else {
            HashSet::new()
        }
    }
}
