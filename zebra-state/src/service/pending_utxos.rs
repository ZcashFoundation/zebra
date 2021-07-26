use std::collections::HashMap;
use std::future::Future;

use tokio::sync::broadcast;

use zebra_chain::transparent::{self, CoinbaseSpendRestriction};

use crate::{service::check, BoxError, Response};

#[derive(Debug, Default)]
pub struct PendingUtxos(HashMap<transparent::OutPoint, broadcast::Sender<transparent::Utxo>>);

impl PendingUtxos {
    /// Returns a future that will resolve to the `transparent::Output` pointed
    /// to by the given `transparent::OutPoint` when it is available.
    pub fn queue(
        &mut self,
        outpoint: transparent::OutPoint,
        spend_restriction: CoinbaseSpendRestriction,
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
            let utxo = receiver.recv().await?;

            check::utxo::validate_transparent_coinbase_spend(outpoint, spend_restriction, utxo)
                .map(Response::SpendableUtxo)
                .map_err(BoxError::from)
        }
    }

    /// Notify all requests waiting for the [`Utxo`] pointed to by the given
    /// [`transparent::OutPoint`] that the [`Utxo`] has arrived.
    pub fn respond(&mut self, outpoint: &transparent::OutPoint, utxo: transparent::Utxo) {
        // TODO: check spend rules here

        if let Some(sender) = self.0.remove(outpoint) {
            // Adding the outpoint as a field lets us crossreference
            // with the trace of the verification that made the request.
            tracing::trace!(?outpoint, "found pending UTXO");
            let _ = sender.send(utxo);
        }
    }

    /// Check the list of pending UTXO requests against the supplied [`OrderedUtxo`] index.
    pub fn check_against_ordered(
        &mut self,
        ordered_utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    ) {
        for (outpoint, ordered_utxo) in ordered_utxos.iter() {
            self.respond(outpoint, ordered_utxo.utxo.clone())
        }
    }

    /// Check the list of pending UTXO requests against the supplied [`Utxo`] index.
    pub fn check_against(&mut self, utxos: &HashMap<transparent::OutPoint, transparent::Utxo>) {
        for (outpoint, utxo) in utxos.iter() {
            self.respond(outpoint, utxo.clone())
        }
    }

    /// Scan the set of waiting utxo requests for channels where all receivers
    /// have been dropped and remove the corresponding sender.
    pub fn prune(&mut self) {
        self.0.retain(|_, chan| chan.receiver_count() > 0);
    }

    /// Returns the number of utxos that are being waited on.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}
