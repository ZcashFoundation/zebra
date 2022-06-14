//! State [`tower::Service`] response types.

use std::{collections::BTreeMap, sync::Arc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block},
    orchard, sapling,
    transaction::{self, Transaction},
    transparent,
};

// Allow *only* this unused import, so that rustdoc link resolution
// will work with inline links.
#[allow(unused_imports)]
use crate::Request;

use crate::{service::read::AddressUtxos, TransactionLocation};

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a [`StateService`][1] [`Request`][2].
///
/// [1]: crate::service::StateService
/// [2]: crate::Request
pub enum Response {
    /// Response to [`Request::CommitBlock`] indicating that a block was
    /// successfully committed to the state.
    Committed(block::Hash),

    /// Response to [`Request::Depth`] with the depth of the specified block.
    Depth(Option<u32>),

    /// Response to [`Request::Tip`] with the current best chain tip.
    Tip(Option<(block::Height, block::Hash)>),

    /// Response to [`Request::BlockLocator`] with a block locator object.
    BlockLocator(Vec<block::Hash>),

    /// Response to [`Request::Transaction`] with the specified transaction.
    Transaction(Option<Arc<Transaction>>),

    /// Response to [`Request::Block`] with the specified block.
    Block(Option<Arc<Block>>),

    /// The response to a `AwaitUtxo` request.
    Utxo(transparent::Utxo),

    /// The response to a `FindBlockHashes` request.
    BlockHashes(Vec<block::Hash>),

    /// The response to a `FindBlockHeaders` request.
    BlockHeaders(Vec<block::CountedHeader>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a read-only
/// [`ReadStateService`](crate::service::ReadStateService)'s
/// [`ReadRequest`](crate::ReadRequest).
pub enum ReadResponse {
    /// Response to [`ReadRequest::Block`](crate::ReadRequest::Block) with the
    /// specified block.
    Block(Option<Arc<Block>>),

    /// Response to
    /// [`ReadRequest::Transaction`](crate::ReadRequest::Transaction) with the
    /// specified transaction.
    Transaction(Option<(Arc<Transaction>, block::Height)>),

    /// Response to
    /// [`ReadRequest::SaplingTree`](crate::ReadRequest::SaplingTree) with the
    /// specified Sapling note commitment tree.
    SaplingTree(Option<Arc<sapling::tree::NoteCommitmentTree>>),

    /// Response to
    /// [`ReadRequest::OrchardTree`](crate::ReadRequest::OrchardTree) with the
    /// specified Orchard note commitment tree.
    OrchardTree(Option<Arc<orchard::tree::NoteCommitmentTree>>),

    /// Response to
    /// [`ReadRequest::AddressBalance`](crate::ReadRequest::AddressBalance) with
    /// the total balance of the addresses.
    AddressBalance(Amount<NonNegative>),

    /// Response to
    /// [`ReadRequest::TransactionIdsByAddresses`](crate::ReadRequest::TransactionIdsByAddresses)
    /// with the obtained transaction ids, in the order they appear in blocks.
    AddressesTransactionIds(BTreeMap<TransactionLocation, transaction::Hash>),

    /// Response to
    /// [`ReadRequest::UtxosByAddresses`](crate::ReadRequest::UtxosByAddresses)
    /// with found utxos and transaction data.
    Utxos(AddressUtxos),
}
