//! Tests for the spentness-hint startup wiring.

use zebra_chain::{
    block, chain_tip::mock::MockChainTip, parameters::spentness_hints::ArtifactEncoder,
    state_snapshot::ObjectHash,
};

use super::*;

/// The test artifact's terminal height.
const TERMINAL_HEIGHT: Height = Height(1_000);

/// The test artifact's terminal block hash.
const TERMINAL_HASH: block::Hash = block::Hash([0x22; 32]);

/// A valid 10-output Mainnet artifact, with outputs 3 and 8 retained.
fn artifact_bytes() -> Vec<u8> {
    let mut encoder = ArtifactEncoder::new(
        Network::Mainnet.genesis_hash(),
        TERMINAL_HEIGHT,
        TERMINAL_HASH,
        10,
    )
    .expect("10 outputs fit an artifact");
    for ordinal in 0..10 {
        encoder
            .push(ordinal == 3 || ordinal == 8)
            .expect("the artifact has 10 bits");
    }
    encoder.finish().expect("every bit was pushed")
}

/// A pin for [`artifact_bytes`], on any network.
fn test_pin(_network: &Network) -> Option<MaxCheckpoint> {
    Some(MaxCheckpoint {
        height: TERMINAL_HEIGHT,
        hash: TERMINAL_HASH,
        spentness_hash: ObjectHash::of(&artifact_bytes()).0,
    })
}

/// No pin, on any network.
fn no_pin(_network: &Network) -> Option<MaxCheckpoint> {
    None
}

/// Saves [`artifact_bytes`] in `artifact_dir`, where acquisition saves it.
fn save_test_artifact(artifact_dir: &Path) {
    let pin = test_pin(&Network::Mainnet).expect("the test pin exists");
    std::fs::create_dir_all(artifact_dir).expect("artifact directory is writable");
    std::fs::write(
        artifact_dir.join(pin.object_hash().to_string()),
        artifact_bytes(),
    )
    .expect("artifact file is writable");
}

/// An empty peer registry.
fn no_peers() -> zebra_network::SyncPeers {
    let (_sender, peers) = watch::channel(Arc::from([]));
    peers
}

/// Hints in `mode` for a state with `tip_height` whose hinted sync is
/// `incomplete` or not, and the state's receiver.
fn state_hints(
    mode: Mode,
    tip_height: Option<Height>,
    incomplete: bool,
) -> (Option<NodeHints>, zebra_state::SpentnessHintsReceiver) {
    let (state_hints, receiver) = StateHints::new(mode).expect("mode uses hints");
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(tip_height);
    (state_hints.for_state(&tip, incomplete), receiver)
}

/// Hints in `mode` for an empty state, and the state's receiver.
fn empty_state_hints(mode: Mode) -> (NodeHints, zebra_state::SpentnessHintsReceiver) {
    let (node_hints, receiver) = state_hints(mode, None, false);
    (node_hints.expect("an empty state uses hints"), receiver)
}

#[test]
fn off_mode_has_no_hints() {
    let _init_guard = zebra_test::init();

    assert!(StateHints::new(Mode::Off).is_none());
    assert!(StateHints::new(Mode::Auto).is_some());
    assert!(StateHints::new(Mode::Require).is_some());
}

/// Hints apply to an empty state, and to a state whose hinted sync a restart
/// interrupted, behind a closed gate. Any other state drops the state's sender.
#[test]
fn hints_apply_only_to_an_empty_or_incomplete_state() {
    let _init_guard = zebra_test::init();

    for (tip_height, incomplete, hinted) in [
        (None, false, true),
        (Some(Height(0)), false, true),
        (Some(Height(1)), false, false),
        (Some(Height(500)), true, true),
    ] {
        let case = format!("tip {tip_height:?}, incomplete {incomplete}");
        let (node_hints, receiver) = state_hints(Mode::Auto, tip_height, incomplete);

        assert_eq!(node_hints.is_some(), hinted, "{case}");
        assert!(receiver.borrow().is_none(), "{case}: no artifact yet");
        match node_hints {
            Some(node_hints) => {
                assert!(
                    !*node_hints.gate().borrow(),
                    "{case}: the gate starts closed"
                );
                assert!(receiver.has_changed().is_ok(), "{case}: the sender is kept");
            }
            None => assert!(
                receiver.has_changed().is_err(),
                "{case}: the sender is dropped"
            ),
        }
    }
}

/// The legacy syncer shares the engine's gate. With `auto` it gives up on the
/// hints task only after the task's own limits have passed; with `require` it
/// waits as long as acquisition takes.
#[test]
fn legacy_syncer_waits_for_the_hints_task() {
    let _init_guard = zebra_test::init();

    assert!(
        LEGACY_SYNC_HINTS_TIMEOUT
            > KNOWN_HASH_SUM_TIMEOUT + crate::components::checkpoint_sync::hints::AUTO_TIMEOUT,
        "the fallback fires only if the hints task is stuck"
    );

    for (mode, timeout) in [
        (Mode::Auto, Some(LEGACY_SYNC_HINTS_TIMEOUT)),
        (Mode::Require, None),
    ] {
        let (node_hints, _receiver) = empty_state_hints(mode);
        let (gate, legacy_timeout) = node_hints.legacy_gate();

        assert_eq!(legacy_timeout, timeout, "{mode:?}");
        assert!(!*gate.borrow(), "{mode:?}: the gate starts closed");
        assert!(
            gate.same_channel(&node_hints.gate()),
            "{mode:?}: the engine and the legacy syncer share the gate"
        );
    }
}

/// Hints need the checkpoint sync engine.
#[test]
fn hints_need_checkpoint_sync() {
    let _init_guard = zebra_test::init();

    for mode in [Mode::Off, Mode::Auto, Mode::Require] {
        assert_eq!(effective_mode(mode, true).expect("the engine is on"), mode);
    }
    assert_eq!(
        effective_mode(Mode::Off, false).expect("off needs nothing"),
        Mode::Off
    );
    assert_eq!(
        effective_mode(Mode::Auto, false).expect("auto falls back"),
        Mode::Off
    );
    assert!(effective_mode(Mode::Require, false).is_err());
}

/// `auto` opens the gate without an artifact when the network has no pin.
#[tokio::test]
async fn auto_without_a_pin_syncs_unhinted() {
    let _init_guard = zebra_test::init();

    let (node_hints, receiver) = empty_state_hints(Mode::Auto);
    let gate = node_hints.gate();
    let dir = tempfile::tempdir().expect("temporary directory");

    node_hints
        .spawn(
            Network::new_regtest(Default::default()),
            no_peers(),
            dir.path().to_owned(),
            None,
        )
        .await
        .expect("task does not panic")
        .expect("auto falls back to syncing without hints");

    assert!(*gate.borrow(), "the gate is open");
    assert!(receiver.borrow().is_none(), "no artifact was sent");
}

/// `require` stops Zebra when the network has no pin.
#[tokio::test]
async fn require_without_a_pin_fails() {
    let _init_guard = zebra_test::init();

    let (node_hints, receiver) = empty_state_hints(Mode::Require);
    let gate = node_hints.gate();
    let dir = tempfile::tempdir().expect("temporary directory");

    let result = node_hints
        .spawn(
            Network::new_regtest(Default::default()),
            no_peers(),
            dir.path().to_owned(),
            None,
        )
        .await
        .expect("task does not panic");

    assert!(result.is_err(), "require fails without a pin");
    assert!(!*gate.borrow(), "the gate stays closed");
    assert!(receiver.borrow().is_none(), "no artifact was sent");
}

/// After a restart part way through a hinted sync, the state is offered the saved
/// artifact before the gate opens, so its writer can resume the `Applying` record.
#[tokio::test]
async fn restart_offers_the_saved_artifact_to_an_incomplete_state() {
    let _init_guard = zebra_test::init();

    for mode in [Mode::Auto, Mode::Require] {
        let (node_hints, receiver) = state_hints(mode, Some(Height(500)), true);
        let node_hints = node_hints.expect("an incomplete state uses hints");
        let gate = node_hints.gate();
        let dir = tempfile::tempdir().expect("temporary directory");
        save_test_artifact(dir.path());

        node_hints
            .run(
                Network::Mainnet,
                test_pin,
                no_peers(),
                dir.path().to_owned(),
                None,
            )
            .await
            .expect("a saved artifact needs no peers");

        assert!(*gate.borrow(), "{mode:?}: the gate is open");
        let artifact = receiver
            .borrow()
            .clone()
            .expect("the saved artifact was sent to the state");
        assert_eq!(artifact.header().terminal_height, TERMINAL_HEIGHT);
        assert_eq!(artifact.bytes().as_ref(), artifact_bytes().as_slice());
    }
}

/// An incomplete state without a saved artifact fetches it again. When that
/// fails, `auto` opens the gate and drops the state's sender, so a resumed
/// writer fails at once instead of waiting for the artifact, and `require`
/// stops Zebra.
#[tokio::test]
async fn incomplete_state_without_a_saved_artifact_fetches_it_again() {
    let _init_guard = zebra_test::init();

    for mode in [Mode::Auto, Mode::Require] {
        let (node_hints, receiver) = state_hints(mode, Some(Height(500)), true);
        let node_hints = node_hints.expect("an incomplete state uses hints");
        let gate = node_hints.gate();
        let dir = tempfile::tempdir().expect("temporary directory");

        // Regtest has no pin, so the fetch fails.
        let result = node_hints
            .run(
                Network::new_regtest(Default::default()),
                test_pin,
                no_peers(),
                dir.path().to_owned(),
                None,
            )
            .await;

        assert_eq!(result.is_ok(), mode == Mode::Auto, "{mode:?}");
        assert_eq!(*gate.borrow(), mode == Mode::Auto, "{mode:?}: the gate");
        assert!(
            receiver.borrow().is_none(),
            "{mode:?}: no artifact was sent"
        );
        assert!(
            receiver.has_changed().is_err(),
            "{mode:?}: the state's sender is dropped"
        );
    }
}

/// A saved file loads only when it matches the current pin.
#[tokio::test]
async fn saved_artifact_loads_only_for_the_current_pin() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("temporary directory");
    assert!(
        load_saved(&Network::Mainnet, test_pin, dir.path())
            .await
            .expect("a missing directory is not an error")
            .is_none(),
        "nothing is saved yet"
    );

    save_test_artifact(dir.path());
    assert!(
        load_saved(&Network::Mainnet, test_pin, dir.path())
            .await
            .expect("the saved artifact verifies")
            .is_some(),
        "the saved artifact loads"
    );
    assert!(
        load_saved(&Network::Mainnet, no_pin, dir.path())
            .await
            .expect("no pin is not an error")
            .is_none(),
        "nothing loads without a pin"
    );
    assert!(
        load_saved(&Network::new_default_testnet(), test_pin, dir.path())
            .await
            .is_err(),
        "an artifact for another chain fails verification"
    );

    // A file with the pin's name but other contents is deleted, so acquisition
    // fetches the artifact again.
    let pin = test_pin(&Network::Mainnet).expect("the test pin exists");
    let path = dir.path().join(pin.object_hash().to_string());
    let mut damaged = artifact_bytes();
    *damaged.last_mut().expect("artifact is not empty") ^= 0x01;
    std::fs::write(&path, damaged).expect("artifact file is writable");
    assert!(
        load_saved(&Network::Mainnet, test_pin, dir.path())
            .await
            .expect("a damaged file is not an error")
            .is_none(),
        "a damaged artifact is not loaded"
    );
    assert!(!path.exists(), "and is deleted");
}

/// Loading the saved artifact for the current pin also deletes the stale files
/// of earlier pins, so a node that already holds the current artifact never
/// keeps or serves a revoked one.
#[tokio::test]
async fn loading_the_saved_artifact_deletes_stale_files() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("temporary directory");
    save_test_artifact(dir.path());
    let stale = ObjectHash::of(b"an artifact of an earlier pin").to_string();
    std::fs::write(dir.path().join(&stale), b"stale").expect("artifact file is writable");
    std::fs::write(dir.path().join(format!("{stale}.tmp")), b"partial")
        .expect("artifact file is writable");

    assert!(
        load_saved(&Network::Mainnet, test_pin, dir.path())
            .await
            .expect("the saved artifact verifies")
            .is_some(),
        "the saved artifact loads"
    );

    let pin = test_pin(&Network::Mainnet).expect("the test pin exists");
    let files: Vec<String> = std::fs::read_dir(dir.path())
        .expect("the directory exists")
        .map(|entry| {
            entry
                .expect("the entry is readable")
                .file_name()
                .into_string()
                .expect("the test names are UTF-8")
        })
        .collect();
    assert_eq!(files, vec![pin.object_hash().to_string()]);
}

/// Version 2 nodes get a served-object index, whatever their hints mode.
#[test]
fn version_2_nodes_serve_objects() {
    let _init_guard = zebra_test::init();

    let mut config = zebra_network::Config::default();
    assert!(object_source(&config).is_none(), "version 2 is off");

    config.v2_listen_addr = Some("127.0.0.1:0".parse().expect("valid address"));
    let (_objects, source) = object_source(&config).expect("version 2 is on");
    assert!(source.borrow().is_empty(), "nothing is served yet");
}
