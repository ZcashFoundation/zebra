//! Dialling version 2 peers for a state-snapshot bootstrap, before the peer
//! set exists.

use std::time::Duration;

use crate::{peer, protocol::v2::types::ErrorCode, quic, PeerSocketAddr};

/// The version 2 peers a state-snapshot bootstrap downloads from, dialled
/// before the peer set exists.
#[derive(Debug)]
pub struct BootstrapPeers {
    /// The connected peers, each as a client the bootstrap sends its requests
    /// to.
    pub clients: Vec<(PeerSocketAddr, peer::Client)>,

    /// The client-only endpoint the connections were dialled from, closed by
    /// [`Self::close`].
    endpoint: quinn::Endpoint,
}

impl BootstrapPeers {
    /// Closes every connection, then the endpoint with `NO_ERROR`, and waits
    /// up to [`BOOTSTRAP_CLOSE_GRACE`] for the peers to acknowledge, so that
    /// the network starts without a lingering endpoint.
    pub async fn close(self) {
        drop(self.clients);
        self.endpoint.close(
            quic::app_error_code(ErrorCode::NoError),
            b"bootstrap finished",
        );
        if tokio::time::timeout(BOOTSTRAP_CLOSE_GRACE, self.endpoint.wait_idle())
            .await
            .is_err()
        {
            tracing::debug!(
                grace = ?BOOTSTRAP_CLOSE_GRACE,
                "bootstrap peers did not acknowledge the close in time"
            );
        }
    }
}

/// How long [`BootstrapPeers::close`] waits for the peers to acknowledge the
/// close.
pub const BOOTSTRAP_CLOSE_GRACE: Duration = Duration::from_secs(2);
