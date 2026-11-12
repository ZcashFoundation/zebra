//! Tests for fast checkpoint sync with peer-supplied note commitment tree roots.

mod commit;

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    history_tree::HistoryTree,
    orchard,
    parallel::tree::{TreeRoots, MAX_TREE_ROOTS_PER_REQUEST},
    parameters::Network,
    sapling,
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
};
use zebra_test::vectors::{MAINNET_BLOCKS, MAINNET_FINAL_SAPLING_ROOTS};

use crate::{CheckpointVerifiedBlock, Config};

use super::*;

/// Returns embedded trees at `height` with empty trees.
fn empty_trees(height: u32) -> Arc<EmbeddedTrees> {
    Arc::new(EmbeddedTrees {
        height: Height(height),
        sapling: Default::default(),
        orchard: Default::default(),
        ironwood: Default::default(),
        sapling_subtrees: Vec::new(),
        orchard_subtrees: Vec::new(),
        ironwood_subtrees: Vec::new(),
    })
}

/// Returns `count` copies of the empty roots.
fn roots(count: usize) -> Vec<TreeRootsProof> {
    vec![
        TreeRootsProof {
            roots: TreeRoots::empty(),
            successor_auth_data_root: None
        };
        count
    ]
}

#[test]
fn cache_keeps_only_the_window_up_to_the_embedded_trees() {
    let mut cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));

    cache.supply(Height(50), roots(100));
    assert_eq!(cache.roots(Height(99)), None, "below the writer");
    assert!(cache.roots(Height(100)).is_some());
    assert!(cache.roots(Height(149)).is_some());

    cache.supply(Height(990), roots(20));
    assert!(cache.roots(Height(1_000)).is_some());
    assert_eq!(cache.roots(Height(1_001)), None, "above the embedded trees");

    // The window ends `TREE_ROOTS_WINDOW` heights after the writer.
    let mut cache = TreeRootsCache::new(Some(empty_trees(100_000)), Height(0));
    cache.supply(Height(TREE_ROOTS_WINDOW - 1), roots(2));
    assert!(cache.roots(Height(TREE_ROOTS_WINDOW - 1)).is_some());
    assert_eq!(cache.roots(Height(TREE_ROOTS_WINDOW)), None);
}

#[test]
fn cache_wants_the_first_missing_run() {
    let mut cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    assert_eq!(cache.wanted(), Some(Height(100)..=Height(1_000)));

    cache.supply(Height(100), roots(10));
    let alternate = "127.0.0.2:8233".parse().unwrap();
    cache.receive((Height(200), roots(10), alternate));
    assert_eq!(cache.wanted(), Some(Height(110)..=Height(199)));

    // Reject all unverified roots from this source, retaining the alternate source's work.
    cache.reject(Height(105));
    assert_eq!(cache.wanted(), Some(Height(100)..=Height(199)));
    assert_eq!(cache.roots(Height(104)), None);
    assert!(cache.roots(Height(200)).is_some());

    cache.receive((Height(100), roots(901), alternate));
    assert_eq!(
        cache.wanted(),
        None,
        "every height up to the embedded trees is supplied"
    );

    let cache =
        TreeRootsCache::new(Some(empty_trees(1_000)), Height(0)).with_first_height(Height(500));
    assert_eq!(
        cache.wanted(),
        Some(Height(500)..=Height(1_000)),
        "fast sync can't use roots below its first height"
    );
}

#[test]
fn cache_finishes_after_the_embedded_trees() {
    let mut cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    cache.supply(Height(900), roots(101));

    cache.set_next_height(Height(950));
    assert_eq!(
        cache.roots(Height(949)),
        None,
        "committed roots are dropped"
    );
    assert!(cache.roots(Height(950)).is_some());
    assert!(cache.embedded_trees().is_some());

    cache.set_next_height(Height(1_001));
    assert!(cache.embedded_trees().is_none());
    assert_eq!(cache.wanted(), None);

    let cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(1_001));
    assert!(
        cache.embedded_trees().is_none(),
        "a state past the embedded trees doesn't fast sync"
    );
}

#[test]
fn writer_receives_roots_over_the_bounded_channel_and_publishes_progress() {
    let mut owner = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    let (sender, wanted, _rejected) = owner.channels();
    let peer = "127.0.0.1:8233".parse().unwrap();
    assert_eq!(*wanted.borrow(), Some(Height(100)..=Height(1_000)));
    let supply = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        sender.send((Height(100), roots(2), peer)).unwrap();
    });
    assert_eq!(
        owner.wait_for_roots(Height(100), Duration::from_secs(1)),
        Some(roots(1)[0])
    );
    supply.join().unwrap();
    assert_eq!(*wanted.borrow(), Some(Height(102)..=Height(1_000)));
    assert_eq!(
        owner.wait_for_roots(Height(102), Duration::from_millis(5)),
        None
    );
}

#[test]
fn roots_input_applies_backpressure_without_discarding_the_pending_batch() {
    let mut owner = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    let (sender, _wanted, _rejected) = owner.channels();
    let peer = "127.0.0.1:8233".parse().unwrap();
    sender.try_send((Height(100), roots(1), peer)).unwrap();
    assert!(matches!(
        sender.try_send((Height(101), roots(1), peer)),
        Err(std::sync::mpsc::TrySendError::Full(_))
    ));
    assert_eq!(owner.roots(Height(100)), Some(roots(1)[0]));
    sender.try_send((Height(101), roots(1), peer)).unwrap();
    assert_eq!(owner.roots(Height(101)), Some(roots(1)[0]));
}

#[test]
fn cache_counts_distinct_root_source_addresses() {
    let mut cache = TreeRootsCache::new(Some(empty_trees(1_000)), Height(100));
    cache.min_sources = MIN_TREE_ROOT_SOURCES;
    let source = |address: &str| address.parse::<SocketAddr>().unwrap();

    cache.receive((Height(100), roots(10), source("127.0.0.1:8233")));
    cache.receive((Height(110), roots(10), source("127.0.0.1:18233")));
    cache.receive((Height(2_000), roots(10), source("127.0.0.2:8233")));
    cache.receive((Height(120), roots(10), source("127.0.0.3:8233")));
    assert!(
        !cache.has_enough_sources(),
        "ports of one address, and roots outside the window, don't count"
    );

    cache.receive((Height(130), roots(10), source("127.0.0.4:8233")));
    assert!(cache.has_enough_sources());

    cache.reject(Height(125));
    assert!(
        !cache.has_enough_sources(),
        "a rejected source no longer counts"
    );
}

#[tokio::test]
async fn state_service_forwards_roots_and_repeats_rejections_until_an_accepted_batch() {
    use tower::ServiceExt;

    use crate::{service::StateService, Request, Response};

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let config = Config {
        fast_tree_sync: true,
        ..Config::ephemeral()
    };
    let (mut state, _, _, _) = StateService::new(config, &network, Height::MAX, 0).await;

    let sapling = NetworkUpgrade::Sapling
        .activation_height(&network)
        .expect("Mainnet activates Sapling");
    let wanted = Some(sapling..=(sapling + i64::from(TREE_ROOTS_WINDOW - 1)).unwrap());
    let peer: SocketAddr = "127.0.0.1:8233".parse().unwrap();
    let alternate: SocketAddr = "127.0.0.2:8233".parse().unwrap();
    let supply = |count, source| Request::SupplyTreeRoots {
        start_height: sapling,
        roots: roots(count),
        source,
    };
    let wanted_response =
        |rejected| Ok::<_, String>(Response::WantedTreeRoots(wanted.clone(), rejected));
    macro_rules! call {
        ($request:expr) => {
            (&mut state)
                .oneshot($request)
                .await
                .map_err(|error| error.to_string())
        };
    }

    assert_eq!(call!(Request::WantedTreeRoots), wanted_response(None));
    let oversized = usize::try_from(MAX_TREE_ROOTS_PER_REQUEST).unwrap() + 1;
    assert!(call!(supply(oversized, peer)).is_err());

    // Stand in for the writer's rejection reports.
    let (report, reports) = mpsc::sync_channel(2);
    state.rejected_tree_roots = reports;
    report.send(peer).unwrap();
    for _ in 0..2 {
        assert_eq!(
            call!(Request::WantedTreeRoots),
            wanted_response(Some(peer)),
            "a rejection repeats until the next accepted batch",
        );
    }
    assert_eq!(call!(supply(1, alternate)), Ok(Response::TreeRootsSupplied));
    assert_eq!(call!(Request::WantedTreeRoots), wanted_response(None));

    // The idle writer hasn't taken the pending batch, so the bounded input refuses the next one
    // without blocking the service, and the refusal keeps the next rejection.
    report.send(alternate).unwrap();
    assert_eq!(
        call!(Request::WantedTreeRoots),
        wanted_response(Some(alternate))
    );
    assert!(call!(supply(1, peer)).is_err());
    assert_eq!(
        call!(Request::WantedTreeRoots),
        wanted_response(Some(alternate))
    );
}

/// Returns a Mainnet block from the test vectors.
fn mainnet_block(height: u32) -> CheckpointVerifiedBlock {
    let block: Arc<Block> = MAINNET_BLOCKS[&height]
        .zcash_deserialize_into()
        .expect("test vectors are valid blocks");
    CheckpointVerifiedBlock::from(block)
}

/// Returns the Mainnet roots after a block from the test vectors, before NU5.
fn mainnet_roots(height: u32) -> TreeRootsProof {
    TreeRootsProof {
        roots: TreeRoots {
            sapling: sapling::tree::Root::try_from(*MAINNET_FINAL_SAPLING_ROOTS[&height])
                .expect("test vectors are valid roots"),
            ..TreeRoots::empty()
        },
        successor_auth_data_root: None,
    }
}

/// Returns an Orchard root that isn't the empty root.
fn non_empty_orchard_root() -> orchard::tree::Root {
    let mut one = [0; 32];
    one[0] = 1;
    let root = orchard::tree::Root::try_from(one).expect("1 is a valid field element");
    assert_ne!(root, TreeRoots::empty().orchard);
    root
}

#[test]
fn roots_before_heartwood_are_verified_by_their_own_header() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let history_tree = Arc::new(HistoryTree::default());
    let block = mainnet_block(419_201);
    let next = mainnet_block(419_202);

    verify_tree_roots(
        &network,
        &block,
        &mainnet_roots(419_201),
        &history_tree,
        &next,
    )
    .expect("the chain's roots are valid");

    let wrong_sapling = mainnet_roots(419_200);
    assert_ne!(wrong_sapling, mainnet_roots(419_201));
    assert!(matches!(
        verify_tree_roots(&network, &block, &wrong_sapling, &history_tree, &next),
        Err(TreeRootsError::SaplingRoot { .. })
    ));

    let early_orchard = TreeRootsProof {
        roots: TreeRoots {
            orchard: non_empty_orchard_root(),
            ..mainnet_roots(419_201).roots
        },
        successor_auth_data_root: None,
    };
    assert!(matches!(
        verify_tree_roots(&network, &block, &early_orchard, &history_tree, &next),
        Err(TreeRootsError::InactivePool {
            pool: "Orchard",
            ..
        })
    ));
}

#[test]
fn roots_from_heartwood_are_verified_by_the_next_header() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    // The history tree starts with the Heartwood activation block.
    let empty_history = Arc::new(HistoryTree::default());
    let block = mainnet_block(903_000);
    let next = mainnet_block(903_001);

    let history_tree = verify_tree_roots(
        &network,
        &block,
        &mainnet_roots(903_000),
        &empty_history,
        &next,
    )
    .expect("the chain's roots are valid");
    assert!(history_tree.hash().is_some());

    let wrong_sapling = mainnet_roots(902_999);
    assert_ne!(wrong_sapling, mainnet_roots(903_000));
    assert!(matches!(
        verify_tree_roots(&network, &block, &wrong_sapling, &empty_history, &next),
        Err(TreeRootsError::Commitment { .. })
    ));
}

#[test]
fn embedded_trees_round_trip_and_check_against_the_chain() {
    let _init_guard = zebra_test::init();
    let mut sapling = sapling::tree::NoteCommitmentTree::default();
    for _ in 0..3 {
        let cm_u = sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&[0; 32])
            .expect("zero is a valid commitment");
        sapling.append(cm_u).expect("the tree has room");
    }
    let roots = TreeRoots {
        sapling: sapling.root(),
        ..TreeRoots::empty()
    };
    let trees = EmbeddedTrees {
        height: Height(1_234),
        sapling: Arc::new(sapling),
        orchard: Default::default(),
        ironwood: Default::default(),
        sapling_subtrees: Vec::new(),
        orchard_subtrees: Vec::new(),
        ironwood_subtrees: Vec::new(),
    };

    let decoded = EmbeddedTrees::from_bytes(&trees.to_bytes()).expect("serialized trees decode");
    assert_eq!(decoded, trees);

    decoded
        .check(&roots, [3, 0, 0])
        .expect("the trees match their roots and sizes");
    assert!(decoded.check(&roots, [4, 0, 0]).is_err(), "wrong size");
    assert!(
        decoded.check(&TreeRoots::empty(), [3, 0, 0]).is_err(),
        "wrong root"
    );

    let mut bytes = trees.to_bytes();
    bytes.push(0);
    assert!(EmbeddedTrees::from_bytes(&bytes).is_err(), "trailing bytes");
    assert!(
        EmbeddedTrees::from_bytes(&trees.to_bytes()[..10]).is_err(),
        "truncated"
    );
}

#[test]
fn embedded_subtrees_are_one_per_full_subtree_in_order() {
    let subtree = |index: u16| NoteCommitmentSubtree::new(index, Height(1), ());
    let full = 1 << 16;

    assert!(subtrees_are_complete::<()>(&[], full - 1));
    assert!(subtrees_are_complete(&[subtree(0)], full));
    assert!(subtrees_are_complete(
        &[subtree(0), subtree(1)],
        2 * full + 5
    ));
    assert!(!subtrees_are_complete::<()>(&[], full), "a missing subtree");
    assert!(
        !subtrees_are_complete(&[subtree(0), subtree(1)], full),
        "an extra subtree"
    );
    assert!(
        !subtrees_are_complete(&[subtree(1), subtree(0)], 2 * full),
        "out of order"
    );
}
