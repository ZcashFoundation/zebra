#![allow(dead_code)]
use crate::{Error, Response};
use std::collections::HashMap;
use std::future::Future;
use tokio::sync::broadcast;
use zebra_chain::transparent;

pub(super) struct PendingUtxos(
    HashMap<transparent::OutPoint, broadcast::Sender<transparent::Output>>,
);

impl PendingUtxos {
    pub(super) fn queue(
        &mut self,
        outpoint: transparent::OutPoint,
    ) -> impl Future<Output = Result<Response, Error>> {
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
                .map_err(Error::from)
        }
    }

    pub(super) fn respond(&mut self, outpoint: transparent::OutPoint, output: transparent::Output) {
        if let Some(sender) = self.0.remove(&outpoint) {
            let _ = sender.send(output);
        }
    }

    pub(super) fn prune(&mut self) {
        self.0.retain(|_, chan| chan.receiver_count() > 0);
    }
}
