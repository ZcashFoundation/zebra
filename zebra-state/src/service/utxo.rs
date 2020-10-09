#![allow(dead_code)]
use crate::{BoxError, Response};
use std::collections::HashMap;
use std::future::Future;
use tokio::sync::broadcast;
use zebra_chain::{block::Block, transparent};

#[derive(Debug, Default)]
pub struct PendingUtxos(HashMap<transparent::OutPoint, broadcast::Sender<transparent::Output>>);

impl PendingUtxos {
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

    pub fn respond(&mut self, outpoint: transparent::OutPoint, output: transparent::Output) {
        if let Some(sender) = self.0.remove(&outpoint) {
            let _ = sender.send(output);
        }
    }

    pub fn check_block(&mut self, block: &Block) {
        if self.0.is_empty() {
            return;
        }

        for transaction in block.transactions.iter() {
            let transaction_hash = transaction.hash();
            for (index, output) in transaction.outputs().iter().enumerate() {
                let outpoint = transparent::OutPoint {
                    hash: transaction_hash,
                    index: index as _,
                };

                self.respond(outpoint, output.clone());
            }
        }
    }

    pub fn prune(&mut self) {
        self.0.retain(|_, chan| chan.receiver_count() > 0);
    }
}
