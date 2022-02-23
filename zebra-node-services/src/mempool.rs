//! The Zebra mempool.
//!
//! A service that manages known unmined Zcash transactions.

use zebra_chain::transaction::{UnminedTx, UnminedTxId};

/// A gossiped transaction, which can be the transaction itself or just its ID.
#[derive(Debug, Eq, PartialEq)]
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
