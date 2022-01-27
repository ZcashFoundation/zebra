//! The peer message sender channel.

use futures::{Sink, SinkExt};

use zebra_chain::serialization::SerializationError;

use crate::{constants::REQUEST_TIMEOUT, protocol::external::Message, PeerError};

/// A wrapper type for a peer connection message sender.
///
/// Used to apply a timeout to send messages.
#[derive(Clone, Debug)]
pub struct PeerTx<Tx> {
    /// A channel for sending Zcash messages to the connected peer.
    ///
    /// This channel accepts [`Message`]s.
    inner: Tx,
}

impl<Tx> PeerTx<Tx>
where
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    /// Sends `msg` on `self.inner`, returning a timeout error if it takes too long.
    pub async fn send(&mut self, msg: Message) -> Result<(), PeerError> {
        tokio::time::timeout(REQUEST_TIMEOUT, self.inner.send(msg))
            .await
            .map_err(|_| PeerError::ClientSendTimeout)?
            .map_err(Into::into)
    }
}

impl<Tx> From<Tx> for PeerTx<Tx> {
    fn from(tx: Tx) -> Self {
        PeerTx { inner: tx }
    }
}
