//! Representation of a gossiped transaction to send to the mempool.

use zebra_chain::transaction::{UnminedTx, UnminedTxId};

/// A gossiped transaction, which can be the transaction itself or just its ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Gossip {
    /// Just the ID of an unmined transaction.
    Id(UnminedTxId),

    /// The full contents of an unmined transaction.
    Tx(UnminedTx),
}

impl Gossip {
    /// Return the [`UnminedTxId`] of a gossiped transaction.
    pub fn id(&self) -> UnminedTxId {
        match self {
            Gossip::Id(txid) => *txid,
            Gossip::Tx(tx) => tx.id,
        }
    }

    /// Return the [`UnminedTx`] of a gossiped transaction, if we have it.
    pub fn tx(&self) -> Option<UnminedTx> {
        match self {
            Gossip::Id(_) => None,
            Gossip::Tx(tx) => Some(tx.clone()),
        }
    }
}

impl From<UnminedTxId> for Gossip {
    fn from(txid: UnminedTxId) -> Self {
        Gossip::Id(txid)
    }
}

impl From<UnminedTx> for Gossip {
    fn from(tx: UnminedTx) -> Self {
        Gossip::Tx(tx)
    }
}
