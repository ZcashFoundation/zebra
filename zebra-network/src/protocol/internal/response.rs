use std::error::Error;

// XXX clean module layout of zebra_chain
use zebra_chain::block::Block;

use crate::meta_addr::MetaAddr;

/// A response to a network request, represented in internal format.
#[derive(Clone, Debug)]
pub enum Response {
    /// Generic success.
    Ok,

    /// Generic error.
    Error,

    /// A list of peers, used to respond to `GetPeers`.
    Peers(Vec<MetaAddr>),

    /// A list of blocks.
    Blocks(Vec<Block>),
}

impl<E> From<E> for Response
where
    E: Error,
{
    fn from(_e: E) -> Self {
        Self::Error
    }
}
