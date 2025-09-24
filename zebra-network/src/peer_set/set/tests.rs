//! Peer set unit tests, and test setup code.

use std::{net::SocketAddr, sync::Arc};

use futures::{channel::mpsc, stream, Stream, StreamExt};
use proptest::{collection::vec, prelude::*};
use proptest_derive::Arbitrary;
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tower::{
    discover::{Change, Discover},
    BoxError,
};
use tracing::Span;

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
};

use crate::{
    address_book::AddressMetrics,
    constants::DEFAULT_MAX_CONNS_PER_IP,
    peer::{ClientTestHarness, LoadTrackedClient, MinimumPeerVersion},
    peer_set::{set::MorePeers, InventoryChange, PeerSet, PeerSetStatus},
    protocol::external::types::Version,
    AddressBook, Config, PeerSocketAddr,
};

#[cfg(test)]
mod prop;

#[cfg(test)]
mod vectors;

/// The maximum number of arbitrary peers to generate in [`PeerVersions`].
///
/// This affects the maximum number of peer connections added to the [`PeerSet`] during the tests.
const MAX_PEERS: usize = 20;

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
    /// The second element is a [`ClientTestHarness`], which contains the open endpoints of the
    /// mock channels used by the peer service.
    ///
    /// The clients and the harnesses are collected into separate [`Vec`] lists and returned.
    pub fn mock_peers(&self) -> (Vec<LoadTrackedClient>, Vec<ClientTestHarness>) {
        let mut clients = Vec::with_capacity(self.peer_versions.len());
        let mut harnesses = Vec::with_capacity(self.peer_versions.len());

        for peer_version in &self.peer_versions {
            let (client, harness) = ClientTestHarness::build()
                .with_version(*peer_version)
                .finish();

            clients.push(client.into());
            harnesses.push(harness);
        }

        (clients, harnesses)
    }

    /// Convert the arbitrary peer versions into mock peer services available through a
    /// [`Discover`] compatible stream.
    ///
    /// A tuple is returned, where the first item is a stream with the mock peers available through
    /// a [`Discover`] interface, and the second is a list of harnesses to the mocked services.
    ///
    /// The returned stream never finishes, so it is ready to be passed to the [`PeerSet`]
    /// constructor.
    ///
    /// See [`Self::mock_peers`] for details on how the peers are mocked and on what the harnesses
    /// contain.
    pub fn mock_peer_discovery(
        &self,
    ) -> (
        impl Stream<Item = Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>>,
        Vec<ClientTestHarness>,
    ) {
        let (clients, harnesses) = self.mock_peers();
        let fake_ports = 1_u16..;

        let discovered_peers_iterator = fake_ports.zip(clients).map(|(port, client)| {
            let peer_address: PeerSocketAddr = SocketAddr::new([127, 0, 0, 1].into(), port).into();

            Ok(Change::Insert(peer_address, client))
        });

        let discovered_peers = stream::iter(discovered_peers_iterator).chain(stream::pending());

        (discovered_peers, harnesses)
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
    inv_stream: Option<broadcast::Receiver<InventoryChange>>,
    address_book: Option<Arc<std::sync::Mutex<AddressBook>>>,
    minimum_peer_version: Option<MinimumPeerVersion<C>>,
    max_conns_per_ip: Option<usize>,
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
            max_conns_per_ip: self.max_conns_per_ip,
        }
    }

    /// Use the provided [`MinimumPeerVersion`] instance when constructing the [`PeerSet`] instance.
    pub fn with_minimum_peer_version<NewC>(
        self,
        minimum_peer_version: MinimumPeerVersion<NewC>,
    ) -> PeerSetBuilder<D, NewC> {
        PeerSetBuilder {
            config: self.config,
            discover: self.discover,
            demand_signal: self.demand_signal,
            handle_rx: self.handle_rx,
            inv_stream: self.inv_stream,
            address_book: self.address_book,
            minimum_peer_version: Some(minimum_peer_version),
            max_conns_per_ip: self.max_conns_per_ip,
        }
    }

    /// Use the provided [`MinimumPeerVersion`] instance when constructing the [`PeerSet`] instance.
    pub fn max_conns_per_ip(self, max_conns_per_ip: usize) -> PeerSetBuilder<D, C> {
        assert!(
            max_conns_per_ip > 0,
            "max_conns_per_ip must be greater than zero"
        );

        PeerSetBuilder {
            config: self.config,
            discover: self.discover,
            demand_signal: self.demand_signal,
            handle_rx: self.handle_rx,
            inv_stream: self.inv_stream,
            address_book: self.address_book,
            minimum_peer_version: self.minimum_peer_version,
            max_conns_per_ip: Some(max_conns_per_ip),
        }
    }
}

impl<D, C> PeerSetBuilder<D, C>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
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
        let max_conns_per_ip = self.max_conns_per_ip;

        let demand_signal = self
            .demand_signal
            .unwrap_or_else(|| guard.create_demand_sender());
        let handle_rx = self
            .handle_rx
            .unwrap_or_else(|| guard.create_background_tasks_receiver());
        let inv_stream = self
            .inv_stream
            .unwrap_or_else(|| guard.create_inventory_receiver());

        let address_metrics = guard.prepare_address_book(self.address_book);
        let (_bans_sender, bans_receiver) = tokio::sync::watch::channel(Default::default());

        let (peer_status_tx, _peer_status_rx) = watch::channel(PeerSetStatus::default());

        let peer_set = PeerSet::new(
            &config,
            discover,
            demand_signal,
            handle_rx,
            inv_stream,
            bans_receiver,
            address_metrics,
            peer_status_tx,
            minimum_peer_version,
            max_conns_per_ip,
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
    inventory_sender: Option<broadcast::Sender<InventoryChange>>,
    address_book: Option<Arc<std::sync::Mutex<AddressBook>>>,
}

impl PeerSetGuard {
    /// Create a new empty [`PeerSetGuard`] instance.
    pub fn new() -> Self {
        PeerSetGuard::default()
    }

    /// Return a mutable reference to the background tasks sender, if present.
    #[allow(dead_code)]
    pub fn background_tasks_sender(
        &mut self,
    ) -> &mut Option<tokio::sync::oneshot::Sender<Vec<JoinHandle<Result<(), BoxError>>>>> {
        &mut self.background_tasks_sender
    }

    /// Return a mutable reference to the background tasks sender, if present.
    #[allow(dead_code)]
    pub fn demand_receiver(&mut self) -> &mut Option<mpsc::Receiver<MorePeers>> {
        &mut self.demand_receiver
    }

    /// Return a mutable reference to the background tasks sender, if present.
    pub fn inventory_sender(&mut self) -> &mut Option<broadcast::Sender<InventoryChange>> {
        &mut self.inventory_sender
    }

    /// Return a mutable reference to the background tasks sender, if present.
    #[allow(dead_code)]
    pub fn address_book(&mut self) -> &mut Option<Arc<std::sync::Mutex<AddressBook>>> {
        &mut self.address_book
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
    pub fn create_inventory_receiver(&mut self) -> broadcast::Receiver<InventoryChange> {
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
    /// Returns a metrics watch channel for the [`AddressBook`] instance tracked by the [`PeerSetGuard`],
    /// so it can be passed to the [`PeerSet`] constructor.
    pub fn prepare_address_book(
        &mut self,
        maybe_address_book: Option<Arc<std::sync::Mutex<AddressBook>>>,
    ) -> watch::Receiver<AddressMetrics> {
        let address_book = maybe_address_book.unwrap_or_else(Self::fallback_address_book);
        let metrics_watcher = address_book
            .lock()
            .expect("unexpected panic in previous address book mutex guard")
            .address_metrics_watcher();

        self.address_book = Some(address_book);

        metrics_watcher
    }

    /// Create an empty [`AddressBook`] instance using a dummy local listener address.
    fn fallback_address_book() -> Arc<std::sync::Mutex<AddressBook>> {
        let local_listener = "127.0.0.1:1000"
            .parse()
            .expect("Invalid local listener address");
        let address_book = AddressBook::new(
            local_listener,
            &Network::Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            Span::none(),
        );

        Arc::new(std::sync::Mutex::new(address_book))
    }
}

/// A pair of block height values, where one is before and the other is at or after an arbitrary
/// network upgrade's activation height.
#[derive(Clone, Debug)]
pub struct BlockHeightPairAcrossNetworkUpgrades {
    /// The network for which the block height values represent heights before and after an
    /// upgrade.
    pub network: Network,

    /// The block height before the network upgrade activation.
    pub before_upgrade: block::Height,

    /// The block height at or after the network upgrade activation.
    pub after_upgrade: block::Height,
}

impl Arbitrary for BlockHeightPairAcrossNetworkUpgrades {
    type Parameters = ();

    fn arbitrary_with((): Self::Parameters) -> Self::Strategy {
        any::<(Network, NetworkUpgrade)>()
            // Filter out genesis upgrade because there is no block height before genesis.
            .prop_filter("no block height before genesis", |(_, upgrade)| {
                !matches!(upgrade, NetworkUpgrade::Genesis)
            })
            // Filter out network upgrades without activation heights.
            .prop_filter_map(
                "missing activation height for network upgrade",
                |(network, upgrade)| {
                    upgrade
                        .activation_height(&network)
                        .map(|height| (network, height))
                },
            )
            // Obtain random heights before and after (or at) the network upgrade activation.
            .prop_flat_map(|(network, activation_height)| {
                let before_upgrade_strategy = 0..activation_height.0;
                let after_upgrade_strategy = activation_height.0..;

                (
                    Just(network),
                    before_upgrade_strategy,
                    after_upgrade_strategy,
                )
            })
            // Collect the arbitrary values to build the final type.
            .prop_map(|(network, before_upgrade_height, after_upgrade_height)| {
                BlockHeightPairAcrossNetworkUpgrades {
                    network,
                    before_upgrade: block::Height(before_upgrade_height),
                    after_upgrade: block::Height(after_upgrade_height),
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
