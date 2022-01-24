//! Networking code for Zebra.
//!
//! ## Network Protocol Design
//!
//! The Zcash network protocol is inherited from Bitcoin, which uses a
//! stateful network protocol in which messages can arrive in any
//! order (even before a handshake is complete!). The same Bitcoin message
//! may be a request or a response depending on context.
//!
//! ### Achieving Concurrency
//!
//! This crate translates the legacy Zcash network protocol
//! into a stateless, request-response oriented protocol defined by
//! the [`Request`] and [`Response`] enums. `zebra-network` completely
//! encapsulates all peer handling code behind a single
//! [`tower::Service`] representing "the network", which load-balances
//! outbound [`Request`]s over available peers.
//!
//! Unlike the underlying legacy network protocol, Zebra's [`PeerSet`]
//! [`tower::Service`] guarantees that each `Request` future will resolve to
//! the correct `Response`, rather than an unrelated `Response` message.
//!
//! Each peer connection is handled by a distinct [`Connection`] task.
//! The Zcash network protocol is bidirectional, so Zebra interprets incoming
//! Zcash messages as either:
//! - [`Response`]s to previously sent outbound [`Request`]s, or
//! - inbound [`Request`]s to an internal [`tower::Service`] representing "this node".
//!
//! All connection state is isolated to individual peers, so this
//! design is structurally immune to the recent `ping` attack.
//!
//! ### Connection Pool
//!
//! Because [`tower::Service`]s provide backpressure information, we
//! can dynamically manage the size of the connection pool according
//! to inbound and outbound demand.  The inbound service can shed load
//! when it is not ready for requests, causing those peer connections
//! to close, and the outbound service can connect to additional peers
//! when it is overloaded.
//!
//! ## `zebra-network` Structure
//!
//! [`zebra-network::init`] is the main entry point for `zebra-network`.
//! It uses the following services, tasks, and endpoints:
//!
//! ### Low-Level Network Connections
//!
//! Inbound Zcash Listener Task:
//!  * accepts inbound connections on the listener port
//!  * initiates Zcash [`Handshake`]s, which creates [`Connection`] tasks for each inbound connection
//!
//! Outbound Zcash Connector Service:
//!  * initiates outbound connections to peer addresses
//!  * initiates Zcash [`Handshake`]s, which creates [`Connection`] tasks for each outbound connection
//!
//! Zebra uses direct TCP connections to share blocks and mempool transactions with other peers.
//!
//! The [`isolated`] APIs provide anonymised TCP and [Tor](https://crates.io/crates/arti)
//! connections to individual peers.
//! These isolated connections can be used to send user-generated transactions anonymously.
//!
//! ### Individual Peer Connections
//!
//! Each new peer connection spawns the following tasks:
//!
//! [`peer::Client`] Service:
//!  * provides an interface for outbound requests to an individual peer
//!    * accepts [`Request`]s assigned to this peer by the [`PeerSet`]
//!    * sends each request to the peer as Zcash [`Message`]
//!    * waits for the inbound response [`Message`] from the peer, and returns it as a [`Response`]
//!
//! [`peer::Connection`] Service:
//!  * manages connection state: awaiting a request, or handling an inbound or outbound response
//!  * provides an interface for inbound requests from an individual peer
//!    * accepts inbound Zcash [`Message`]s from this peer
//!    * handles each message as a [`Request`] to the inbound service
//!    * sends the [`Response`] to the peer as Zcash [`Message`]s
//!  * drops peer connections if the inbound request queue is overloaded
//!
//! Since the Zcash network protocol is bidirectional,
//! inbound and outbound connections are handled using the same logic.
//!
//! ### Connection Pool
//!
//! [`PeerSet`] Network Service:
//!  * provides an interface for other services and tasks running within this node
//!    to make requests to remote peers ("the rest of the network")
//!    * accepts [`Request`]s from the local node
//!    * sends each request to a [`peer::Client`] using randomised load-balancing
//!    * returns the [`Response`] from the [`peer::Client`]
//!
//! Inbound Network Service:
//!  * provides an interface for remote peers to request data held by this node
//!    * accepts inbound Zcash [`Request`]s from [`peer::Connection`]s
//!    * handles each message as a [`Request`] to the local node
//!    * sends the [`Response`] to the [`peer::Connection`]
//!
//! Note: the inbound service is implemented by the [`zebra-network::init`] caller.
//!
//! Peer Inventory Service:
//!  * tracks gossiped `inv` advertisements for each peer
//!  * tracks missing inventory for each peer
//!  * used by the [`PeerSet`] to route block and transaction requests to peers that have the requested data
//!
//! ### Peer Discovery
//!
//! [`AddressBook`] Service:
//!  * maintains a list of peer addresses and associated connection attempt metadata
//!  * address book metadata is used to prioritise peer connection attempts
//!
//! Initial Seed Peer Task:
//!  * initiates new outbound peer connections to seed peers, resolving them via DNS if required
//!  * adds seed peer addresses to the [`AddressBook`]
//!
//! Peer Crawler Task:
//!  * discovers new peer addresses by sending [`Addr`] requests to connected peers
//!  * initiates new outbound peer connections in response to application demand

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_network")]
// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![deny(rust_2021_compatibility)]
#![forbid(unsafe_code)]

#[macro_use]
extern crate pin_project;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate tracing;
#[macro_use]
extern crate bitflags;

/// Type alias to make working with tower traits easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod address_book;
mod address_book_updater;
mod config;
pub mod constants;
mod isolated;
mod meta_addr;
mod peer;
mod peer_set;
mod policies;
mod protocol;

#[cfg(feature = "tor")]
pub use crate::isolated::tor::connect_isolated_tor;

pub use crate::{
    address_book::AddressBook,
    config::Config,
    isolated::{connect_isolated, connect_isolated_tcp_direct},
    meta_addr::PeerAddrState,
    peer::{HandshakeError, PeerError, SharedPeerError},
    peer_set::init,
    policies::RetryLimit,
    protocol::internal::{Request, Response},
};

/// Types used in the definition of [`Request`] and [`Response`] messages.
pub mod types {
    pub use crate::{meta_addr::MetaAddr, protocol::types::PeerServices};
}
