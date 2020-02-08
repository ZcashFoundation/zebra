use std::collections::HashSet;

use zebra_chain::block::BlockHeaderHash;

use super::super::types::Nonce;

/// A network request, represented in internal format.
#[derive(Clone, Debug)]
pub enum Request {
    /// Requests additional peers from the server.
    Peers,

    /// Heartbeats triggered on peer connection start.
    ///
    /// This is included as a bit of a hack, it should only be used
    /// internally for connection management. You should not expect to
    /// be firing or handling `Ping` requests or `Pong` responses.
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
    BlocksByHash(HashSet<BlockHeaderHash>),

    /// Request block hashes of subsequent blocks in the chain, giving hashes of
    /// known blocks.
    FindBlocks {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<BlockHeaderHash>,
        /// Optionally, the last header to request.
        stop: Option<BlockHeaderHash>,
    },
}
