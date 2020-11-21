#![allow(dead_code)]
use crate::{BoxError, Response};
use std::collections::HashMap;
use std::future::Future;
use tokio::sync::broadcast;
use zebra_chain::{block::Block, transparent};

#[derive(Debug, Default)]
pub struct PendingUtxos(HashMap<transparent::OutPoint, broadcast::Sender<transparent::Output>>);

impl PendingUtxos {
    /// Returns a future that will resolve to the `transparent::Output` pointed
    /// to by the given `transparent::OutPoint` when it is available.
    pub fn queue(
        &mut self,
        outpoint: transparent::OutPoint,
    ) -> impl Future<Output = Result<Response, BoxError>> {
        let mut receiver = self
            .0
            .entry(outpoint)
            .or_insert_with(|| {
                let (sender, _) = broadcast::channel(1);
                sender
            })
            .subscribe();

        async move {
            receiver
                .recv()
                .await
                .map(Response::Utxo)
                .map_err(BoxError::from)
        }
    }

    /// Notify all utxo requests waiting for the `transparent::Output` pointed to
    /// by the given `transparent::OutPoint` that the `Output` has arrived.
    pub fn respond(&mut self, outpoint: &transparent::OutPoint, output: transparent::Output) {
        if let Some(sender) = self.0.remove(&outpoint) {
            // Adding the outpoint as a field lets us crossreference
            // with the trace of the verification that made the request.
            tracing::trace!(?outpoint, "found pending UTXO");
            let _ = sender.send(output);
        }
    }

    /// Check the list of pending UTXO requests against the supplied UTXO index.
    pub fn check_against(&mut self, utxos: &HashMap<transparent::OutPoint, transparent::Output>) {
        for (outpoint, output) in utxos.iter() {
            self.respond(outpoint, output.clone());
        }
    }

    /// Scan through unindexed transactions in the given `block`
    /// to determine whether it contains any requested UTXOs.
    pub fn scan_block(&mut self, block: &Block) {
        if self.0.is_empty() {
            return;
        }

        tracing::trace!("scanning new block for pending UTXOs");
        for transaction in block.transactions.iter() {
            let transaction_hash = transaction.hash();
            for (index, output) in transaction.outputs().iter().enumerate() {
                let outpoint = transparent::OutPoint {
                    hash: transaction_hash,
                    index: index as _,
                };

                self.respond(&outpoint, output.clone());
            }
        }
    }

    /// Scan the set of waiting utxo requests for channels where all recievers
    /// have been dropped and remove the corresponding sender.
    pub fn prune(&mut self) {
        self.0.retain(|_, chan| chan.receiver_count() > 0);
    }
}
