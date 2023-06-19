//! Pending UTXO tracker for [`AwaitUtxo` requests](crate::Request::AwaitUtxo).

use std::{collections::HashMap, future::Future};

use tokio::sync::broadcast;

use zebra_chain::transparent;

use crate::{BoxError, Response};

#[derive(Debug, Default)]
pub struct PendingUtxos(HashMap<transparent::OutPoint, broadcast::Sender<transparent::Utxo>>);

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

    /// Notify all requests waiting for the [`transparent::Utxo`] pointed to by
    /// the given [`transparent::OutPoint`] that the [`transparent::Utxo`] has
    /// arrived.
    #[inline]
    pub fn respond(&mut self, outpoint: &transparent::OutPoint, utxo: transparent::Utxo) {
        if let Some(sender) = self.0.remove(outpoint) {
            // Adding the outpoint as a field lets us cross-reference
            // with the trace of the verification that made the request.
            tracing::trace!(?outpoint, "found pending UTXO");
            let _ = sender.send(utxo);
        }
    }

    /// Check the list of pending UTXO requests against the supplied
    /// [`transparent::OrderedUtxo`] index.
    pub fn check_against_ordered(
        &mut self,
        ordered_utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    ) {
        for (outpoint, ordered_utxo) in ordered_utxos.iter() {
            self.respond(outpoint, ordered_utxo.utxo.clone())
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
