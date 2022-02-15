//! Peer handling.

mod client;
mod connection;
mod connector;
mod error;
mod handshake;
mod load_tracked_client;
mod minimum_peer_version;

#[cfg(any(test, feature = "proptest-impl"))]
pub use client::tests::ClientTestHarness;

#[cfg(test)]
pub(crate) use client::tests::ReceiveRequestAttempt;
#[cfg(test)]
pub(crate) use handshake::register_inventory_status;

use client::{ClientRequestReceiver, InProgressClientRequest, MustUseClientResponseSender};

pub(crate) use client::{CancelHeartbeatTask, ClientRequest};

pub use client::Client;
pub use connection::Connection;
pub use connector::{Connector, OutboundConnectorRequest};
pub use error::{ErrorSlot, HandshakeError, PeerError, SharedPeerError};
pub use handshake::{ConnectedAddr, Handshake, HandshakeRequest};
pub use load_tracked_client::LoadTrackedClient;
pub use minimum_peer_version::MinimumPeerVersion;
