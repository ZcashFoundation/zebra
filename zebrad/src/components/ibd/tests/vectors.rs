//! Fixed test vectors for the known-hash IBD engine skeleton.

use zebra_chain::{
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::{known_hashes::KnownHashListSpec, Network},
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::{
        ibd::{
            engine::{Engine, IBD_SPAN_MAX},
            DeclineReason, IbdEngine, IbdOutcome,
        },
        sync,
    },
    BoxError,
};

/// A mocked peer set service for the engine.
type MockPeerSet = MockService<zn::Request, zn::Response, PanicAssertion>;

/// A mocked state service for the engine.
type MockState = MockService<zs::Request, zs::Response, PanicAssertion>;

/// Returns mocked peer set and state services, and a mock chain tip with its
/// sender.
///
/// The supervisor preconditions never call the services, so the mocks assert
/// that the skeleton leaves them untouched: any request panics.
fn mock_services() -> (MockPeerSet, MockState, MockChainTip, MockChainTipSender) {
    let peer_set = MockService::build().for_unit_tests();
    let state = MockService::build().for_unit_tests();
    let (chain_tip, chain_tip_sender) = MockChainTip::new();

    (peer_set, state, chain_tip, chain_tip_sender)
}

/// The new known-hash config fields parse from an empty `[sync]` section with
/// their documented defaults, so existing configs keep working under
/// `deny_unknown_fields`.
#[test]
fn known_hash_config_defaults_parse_from_empty_section() {
    let _init_guard = zebra_test::init();

    let config: sync::Config = toml::from_str("")
        .expect("an empty [sync] section parses, because every field has a serde default");

    assert_eq!(config, sync::Config::default());

    assert!(!config.known_hash_sync, "the engine must default off");
    assert_eq!(config.known_hash_lookahead_bytes, 268_435_456);
    assert_eq!(config.known_hash_gap_hedge_secs, 5);
    assert_eq!(config.known_hash_list_dir, None);
    assert!(config.known_hash_list_download);
    assert!(!config.known_hash_cache_write_ahead);
}

/// With the flag off (the default), the supervisor declines immediately,
/// without touching the network, the state, or the list assets.
#[tokio::test]
async fn flag_off_declines_disabled_by_config() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let (peer_set, state, chain_tip, _chain_tip_sender) = mock_services();

    let ibd = IbdEngine::new(
        sync::Config::default(),
        Network::Mainnet,
        peer_set,
        state,
        chain_tip,
    );

    let outcome = ibd.run().await?;

    assert_eq!(
        outcome,
        IbdOutcome::Declined(DeclineReason::DisabledByConfig)
    );

    Ok(())
}

/// With the flag on but no bundled list for the network, the supervisor
/// declines with `NoList`.
#[tokio::test]
async fn no_list_network_declines() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    assert!(
        KnownHashListSpec::for_network(&network).is_none(),
        "this test needs a network without a bundled list; \
         move it to Regtest once the Testnet list lands",
    );

    let (peer_set, state, chain_tip, _chain_tip_sender) = mock_services();

    let config = sync::Config {
        known_hash_sync: true,
        ..sync::Config::default()
    };

    let ibd = IbdEngine::new(config, network, peer_set, state, chain_tip);

    let outcome = ibd.run().await?;

    assert_eq!(outcome, IbdOutcome::Declined(DeclineReason::NoList));

    Ok(())
}

/// With the flag on and the tip at the end of the list, the supervisor
/// declines with `AlreadyPast` from the spec constant alone, without any
/// chunk file I/O.
#[tokio::test]
async fn tip_at_list_max_declines_already_past() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let spec =
        KnownHashListSpec::for_network(&network).expect("Mainnet has a bundled known-hash list");

    let (peer_set, state, chain_tip, chain_tip_sender) = mock_services();
    chain_tip_sender.send_best_tip_height(spec.max_height);

    let config = sync::Config {
        known_hash_sync: true,
        ..sync::Config::default()
    };

    let ibd = IbdEngine::new(config, network, peer_set, state, chain_tip);

    let outcome = ibd.run().await?;

    assert_eq!(outcome, IbdOutcome::Declined(DeclineReason::AlreadyPast));

    Ok(())
}

/// With the flag on, a bundled list, and an empty state, the supervisor
/// verifies the assets, constructs the engine, and gets the D1 stub's
/// `NotYetImplemented` decline.
///
/// Reads and hashes the full Mainnet asset set (~103 MB) from the
/// development tree, which takes about 2 seconds in debug builds.
#[tokio::test]
async fn mainnet_engine_stub_declines_not_yet_implemented() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    // The mock tip starts at `None`, matching an empty state.
    let (peer_set, state, chain_tip, _chain_tip_sender) = mock_services();

    let config = sync::Config {
        known_hash_sync: true,
        ..sync::Config::default()
    };

    let ibd = IbdEngine::new(config, Network::Mainnet, peer_set, state, chain_tip);

    let outcome = ibd.run().await?;

    assert_eq!(
        outcome,
        IbdOutcome::Declined(DeclineReason::NotYetImplemented)
    );

    Ok(())
}

/// The engine's ring window is fully allocated at construction and starts
/// empty: the span-cap invariant depends on the capacity never growing.
#[tokio::test]
async fn engine_ring_is_preallocated_and_empty() {
    let _init_guard = zebra_test::init();

    let (peer_set, state, _chain_tip, _chain_tip_sender) = mock_services();
    let (list, _blocks) = super::FakeHashList::continuous_mainnet(1);
    let (_status_sender, peer_status) =
        tokio::sync::watch::channel(zebra_network::PeerSetStatus::default());

    let engine = Engine::new(
        peer_set,
        state,
        zebra_chain::block::Height(0),
        list,
        peer_status,
        sync::Config::default().known_hash_lookahead_bytes,
        std::time::Duration::from_secs(5),
    );

    assert!(
        engine.window_capacity() >= IBD_SPAN_MAX,
        "the ring must be preallocated with at least IBD_SPAN_MAX slots",
    );
    assert_eq!(engine.window_len(), 0, "the window starts empty");
    assert_eq!(engine.base(), zebra_chain::block::Height(0));
}
