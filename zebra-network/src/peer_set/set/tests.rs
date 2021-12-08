use std::{net::SocketAddr, sync::Arc};

use futures::{
    channel::{mpsc, oneshot},
    stream, Stream, StreamExt,
};
use proptest::{collection::vec, prelude::any};
use proptest_derive::Arbitrary;
use tokio::{sync::broadcast, task::JoinHandle};
use tower::{
    discover::{Change, Discover},
    BoxError,
};
use tracing::Span;

use zebra_chain::chain_tip::ChainTip;

use super::MorePeers;
use crate::{
    peer::{Client, ClientRequest, ErrorSlot, LoadTrackedClient, MinimumPeerVersion},
    peer_set::PeerSet,
    protocol::external::{types::Version, InventoryHash},
    AddressBook, Config,
};

/// The maximum number of arbitrary peers to generate in [`PeerVersions`].
///
/// This affects the maximum number of peer connections added to the [`PeerSet`] during the tests.
const MAX_PEERS: usize = 20;

/// A handle to a mocked [`Client`] instance.
struct MockedClientHandle {
    _request_receiver: mpsc::Receiver<ClientRequest>,
    shutdown_receiver: oneshot::Receiver<()>,
    version: Version,
}

impl MockedClientHandle {
    /// Create a new mocked [`Client`] instance, returning it together with a handle to track it.
    pub fn new(version: Version) -> (Self, LoadTrackedClient) {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let (request_sender, _request_receiver) = mpsc::channel(1);

        let client = Client {
            shutdown_tx: Some(shutdown_sender),
            server_tx: request_sender,
            error_slot: ErrorSlot::default(),
            version,
        };

        let handle = MockedClientHandle {
            _request_receiver,
            shutdown_receiver,
            version,
        };

        (handle, client.into())
    }

    /// Gets the peer protocol version associated to the [`Client`].
    pub fn version(&self) -> Version {
        self.version
    }

    /// Checks if the [`Client`] instance has not been dropped, which would have disconnected from
    /// the peer.
    pub fn is_connected(&mut self) -> bool {
        match self.shutdown_receiver.try_recv() {
            Ok(None) => true,
            Ok(Some(())) | Err(oneshot::Canceled) => false,
        }
    }
}

/// A helper type to generate arbitrary peer versions which can then become mock peer services.
#[derive(Arbitrary, Debug)]
struct PeerVersions {
    #[proptest(strategy = "vec(any::<Version>(), 1..MAX_PEERS)")]
    peer_versions: Vec<Version>,
}

impl PeerVersions {
    /// Convert the arbitrary peer versions into mock peer services.
    ///
    /// Each peer versions results in a mock peer service, which is returned as a tuple. The first
    /// element is the [`LeadTrackedClient`], which is the actual service for the peer connection.
    /// The second element is a [`MockedClientHandle`], which contains the open endpoints of the
    /// mock channels used by the peer service.
    pub fn mock_peers(&self) -> (Vec<LoadTrackedClient>, Vec<MockedClientHandle>) {
        let mut clients = Vec::with_capacity(self.peer_versions.len());
        let mut handles = Vec::with_capacity(self.peer_versions.len());

        for peer_version in &self.peer_versions {
            let (handle, client) = MockedClientHandle::new(*peer_version);

            clients.push(client);
            handles.push(handle);
        }

        (clients, handles)
    }

    /// Convert the arbitrary peer versions into mock peer services available through a
    /// [`Discover`] compatible stream.
    ///
    /// A tuple is returned, where the first item is a stream with the mock peers available through
    /// a [`Discover`] interface, and the second is a list of handles to the mocked services.
    ///
    /// The returned stream never finishes, so it is ready to be passed to the [`PeerSet`]
    /// constructor.
    ///
    /// See [`Self::mock_peers`] for details on how the peers are mocked and on what the handles
    /// contain.
    pub fn mock_peer_discovery(
        &self,
    ) -> (
        impl Stream<Item = Result<Change<SocketAddr, LoadTrackedClient>, BoxError>>,
        Vec<MockedClientHandle>,
    ) {
        let (clients, handles) = self.mock_peers();
        let fake_ports = 1_u16..;

        let discovered_peers_iterator = fake_ports.zip(clients).map(|(port, client)| {
            let peer_address = SocketAddr::new([127, 0, 0, 1].into(), port);

            Ok(Change::Insert(peer_address, client))
        });

        let discovered_peers = stream::iter(discovered_peers_iterator).chain(stream::pending());

        (discovered_peers, handles)
    }
}

/// A helper builder type for creating test [`PeerSet`] instances.
///
/// This helps to reduce repeated boilerplate code. Fields that are not set are configured to use
/// dummy fallbacks.
#[derive(Default)]
struct PeerSetBuilder<D, C> {
    config: Option<Config>,
    discover: Option<D>,
    demand_signal: Option<mpsc::Sender<MorePeers>>,
    handle_rx: Option<tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>>>,
    inv_stream: Option<broadcast::Receiver<(InventoryHash, SocketAddr)>>,
    address_book: Option<Arc<std::sync::Mutex<AddressBook>>>,
    minimum_peer_version: Option<MinimumPeerVersion<C>>,
}

impl PeerSetBuilder<(), ()> {
    /// Create a new [`PeerSetBuilder`] instance.
    pub fn new() -> Self {
        PeerSetBuilder::default()
    }
}

impl<D, C> PeerSetBuilder<D, C> {
    /// Use the provided `discover` parameter when constructing the [`PeerSet`] instance.
    pub fn with_discover<NewD>(self, discover: NewD) -> PeerSetBuilder<NewD, C> {
        PeerSetBuilder {
            discover: Some(discover),
            config: self.config,
            demand_signal: self.demand_signal,
            handle_rx: self.handle_rx,
            inv_stream: self.inv_stream,
            address_book: self.address_book,
            minimum_peer_version: self.minimum_peer_version,
        }
    }

    /// Use the provided [`MinimumPeerVersion`] instance when constructing the [`PeerSet`] instance.
    pub fn with_minimum_peer_version<NewC>(
        self,
        minimum_peer_version: MinimumPeerVersion<NewC>,
    ) -> PeerSetBuilder<D, NewC> {
        PeerSetBuilder {
            minimum_peer_version: Some(minimum_peer_version),
            config: self.config,
            discover: self.discover,
            demand_signal: self.demand_signal,
            handle_rx: self.handle_rx,
            inv_stream: self.inv_stream,
            address_book: self.address_book,
        }
    }
}

impl<D, C> PeerSetBuilder<D, C>
where
    D: Discover<Key = SocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    /// Finish building the [`PeerSet`] instance.
    ///
    /// Returns a tuple with the [`PeerSet`] instance and a [`PeerSetGuard`] to keep track of some
    /// endpoints of channels created for the [`PeerSet`].
    pub fn build(self) -> (PeerSet<D, C>, PeerSetGuard) {
        let mut guard = PeerSetGuard::new();

        let config = self.config.unwrap_or_default();
        let discover = self.discover.expect("`discover` must be set");
        let minimum_peer_version = self
            .minimum_peer_version
            .expect("`minimum_peer_version` must be set");

        let demand_signal = self
            .demand_signal
            .unwrap_or_else(|| guard.create_demand_sender());
        let handle_rx = self
            .handle_rx
            .unwrap_or_else(|| guard.create_background_tasks_receiver());
        let inv_stream = self
            .inv_stream
            .unwrap_or_else(|| guard.create_inventory_receiver());

        let address_book = guard.prepare_address_book(self.address_book);

        let peer_set = PeerSet::new(
            &config,
            discover,
            demand_signal,
            handle_rx,
            inv_stream,
            address_book,
            minimum_peer_version,
        );

        (peer_set, guard)
    }
}

/// A helper type to keep track of some dummy endpoints sent to a test [`PeerSet`] instance.
#[derive(Default)]
pub struct PeerSetGuard {
    background_tasks_sender:
        Option<tokio::sync::oneshot::Sender<Vec<JoinHandle<Result<(), BoxError>>>>>,
    demand_receiver: Option<mpsc::Receiver<MorePeers>>,
    inventory_sender: Option<broadcast::Sender<(InventoryHash, SocketAddr)>>,
    address_book: Option<Arc<std::sync::Mutex<AddressBook>>>,
}

impl PeerSetGuard {
    /// Create a new empty [`PeerSetGuard`] instance.
    pub fn new() -> Self {
        PeerSetGuard::default()
    }

    /// Create a dummy channel for the background tasks sent to the [`PeerSet`].
    ///
    /// The sender is stored inside the [`PeerSetGuard`], while the receiver is returned to be
    /// passed to the [`PeerSet`] constructor.
    pub fn create_background_tasks_receiver(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();

        self.background_tasks_sender = Some(sender);

        receiver
    }

    /// Create a dummy channel for the [`PeerSet`] to send demand signals for more peers.
    ///
    /// The receiver is stored inside the [`PeerSetGuard`], while the sender is returned to be
    /// passed to the [`PeerSet`] constructor.
    pub fn create_demand_sender(&mut self) -> mpsc::Sender<MorePeers> {
        let (sender, receiver) = mpsc::channel(1);

        self.demand_receiver = Some(receiver);

        sender
    }

    /// Create a dummy channel for the inventory hashes sent to the [`PeerSet`].
    ///
    /// The sender is stored inside the [`PeerSetGuard`], while the receiver is returned to be
    /// passed to the [`PeerSet`] constructor.
    pub fn create_inventory_receiver(
        &mut self,
    ) -> broadcast::Receiver<(InventoryHash, SocketAddr)> {
        let (sender, receiver) = broadcast::channel(1);

        self.inventory_sender = Some(sender);

        receiver
    }

    /// Prepare an [`AddressBook`] instance to send to the [`PeerSet`].
    ///
    /// If the `maybe_address_book` parameter contains an [`AddressBook`] instance, it is stored
    /// inside the [`PeerSetGuard`] to keep track of it. Otherwise, a new instance is created with
    /// the [`Self::fallback_address_book`] method.
    ///
    /// A reference to the [`AddressBook`] instance tracked by the [`PeerSetGuard`] is returned to
    /// be passed to the [`PeerSet`] constructor.
    pub fn prepare_address_book(
        &mut self,
        maybe_address_book: Option<Arc<std::sync::Mutex<AddressBook>>>,
    ) -> Arc<std::sync::Mutex<AddressBook>> {
        let address_book = maybe_address_book.unwrap_or_else(Self::fallback_address_book);

        self.address_book = Some(address_book.clone());

        address_book
    }

    /// Create an empty [`AddressBook`] instance using a dummy local listener address.
    fn fallback_address_book() -> Arc<std::sync::Mutex<AddressBook>> {
        let local_listener = "127.0.0.1:1000"
            .parse()
            .expect("Invalid local listener address");
        let address_book = AddressBook::new(local_listener, Span::none());

        Arc::new(std::sync::Mutex::new(address_book))
    }
}
