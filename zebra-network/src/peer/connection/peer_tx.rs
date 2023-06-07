//! The peer message sender channel.

use futures::{FutureExt, Sink, SinkExt};

use zebra_chain::serialization::SerializationError;

use crate::{constants::REQUEST_TIMEOUT, protocol::external::Message, PeerError};

/// A wrapper type for a peer connection message sender.
///
/// Used to apply a timeout to send messages.
#[derive(Clone, Debug)]
pub struct PeerTx<Tx>
where
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
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
            .map_err(|_| PeerError::ConnectionSendTimeout)?
            .map_err(Into::into)
    }

    /// Flush any remaining output and close this [`PeerTx`], if necessary.
    pub async fn close(&mut self) -> Result<(), SerializationError> {
        self.inner.close().await
    }
}

impl<Tx> From<Tx> for PeerTx<Tx>
where
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    fn from(tx: Tx) -> Self {
        PeerTx { inner: tx }
    }
}

impl<Tx> Drop for PeerTx<Tx>
where
    Tx: Sink<Message, Error = SerializationError> + Unpin,
{
    fn drop(&mut self) {
        // Do a last-ditch close attempt on the sink
        self.close().now_or_never();
    }
}
