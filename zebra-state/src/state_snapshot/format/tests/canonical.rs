//! Tests for the canonical values of snapshot components: every legacy
//! encoding the state still reads re-encodes to the current one.

use std::{collections::BTreeMap, sync::Arc};

use bincode::Options;
use serde_big_array::BigArray;

use zebra_chain::{
    amount::NonNegative,
    block::{Block, Height},
    block_info::BlockInfo,
    history_tree::NonEmptyHistoryTree,
    orchard,
    parameters::{Network, NetworkKind},
    primitives::zcash_history::BlockCommitmentTreeRoots,
    sapling,
    serialization::ZcashDeserializeInto,
    sprout,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{HistoryTreeParts, IntoDisk},
    state_snapshot::format::{canonical_value, window_start, ComponentId, FormatError, WINDOW_LEN},
};

/// Asserts that `$result` is an error matching `$pattern`.
macro_rules! assert_err {
    ($result:expr, $pattern:pat $(if $guard:expr)? $(,)?) => {{
        let error = $result.expect_err(concat!("expected an error matching ", stringify!($pattern)));
        assert!(
            matches!(error, $pattern $(if $guard)?),
            "expected {}, got {error:?}",
            stringify!($pattern)
        );
    }};
}

/// The little-endian encodings of the amounts `1..=count` zatoshis, one after
/// another.
fn amounts(count: i64) -> Vec<u8> {
    (1..=count)
        .flat_map(|zatoshis| zatoshis.to_le_bytes())
        .collect()
}

#[test]
fn window_start_is_the_node_network_limited_window() {
    let _init_guard = zebra_test::init();

    assert_eq!(WINDOW_LEN, 2304);
    for (height, start) in [
        (1, 1),
        (2, 1),
        (2303, 1),
        (2304, 1),
        (2305, 2),
        (10_000, 10_000 - 2303),
        (3_522_060, 3_522_060 - 2303),
        (Height::MAX.0, Height::MAX.0 - 2303),
    ] {
        assert_eq!(window_start(Height(height)), Height(start), "{height}");
    }

    // A full window is exactly `WINDOW_LEN` blocks.
    let height = Height(1_000_000);
    assert_eq!(height.0 - window_start(height).0 + 1, WINDOW_LEN);
}

#[test]
fn value_pools_widen_to_the_current_layout() {
    let _init_guard = zebra_test::init();

    let current = amounts(7);
    assert_eq!(current.len(), 56);
    assert_eq!(
        canonical_value(ComponentId::ValuePools, &[], &current).expect("valid"),
        current,
    );

    // Each older layout lacks the newest pools, which are zero.
    for len in [32, 40, 48] {
        let mut expected = current[..len].to_vec();
        expected.resize(56, 0);
        assert_eq!(
            canonical_value(ComponentId::ValuePools, &[], &current[..len]).expect("valid"),
            expected,
            "{len} byte layout",
        );
    }

    for len in [0, 8, 31, 33, 55, 57, 64] {
        assert_err!(
            canonical_value(ComponentId::ValuePools, &[], &amounts(8)[..len]),
            FormatError::InvalidValue {
                component: ComponentId::ValuePools,
                ..
            },
        );
    }

    let mut negative = current.clone();
    negative[8..16].copy_from_slice(&(-1_i64).to_le_bytes());
    assert_err!(
        canonical_value(ComponentId::ValuePools, &[], &negative),
        FormatError::InvalidValue { .. },
    );
}

#[test]
fn block_info_widens_to_the_current_layout() {
    let _init_guard = zebra_test::init();

    let size = 1_234_567_u32.to_le_bytes();
    let pools = amounts(7);
    let block_info = |pools: &[u8]| {
        BlockInfo::new(
            ValueBalance::<NonNegative>::from_bytes(pools).expect("valid pools"),
            u32::from_le_bytes(size),
        )
        .as_bytes()
        .to_vec()
    };

    // The current layout: six pools, the size, then the NSM pool.
    let mut current = pools[..48].to_vec();
    current.extend_from_slice(&size);
    current.extend_from_slice(&pools[48..]);
    assert_eq!(current, block_info(&pools));
    assert_eq!(
        canonical_value(ComponentId::BlockInfo, &[], &current).expect("valid"),
        current,
    );

    // Format 28: six pools and the size.
    let mut format_28 = pools[..48].to_vec();
    format_28.extend_from_slice(&size);
    assert_eq!(
        canonical_value(ComponentId::BlockInfo, &[], &format_28).expect("valid"),
        block_info(&pools[..48]),
    );

    // Before NU6.3: five pools and the size.
    let mut pre_ironwood = pools[..40].to_vec();
    pre_ironwood.extend_from_slice(&size);
    assert_eq!(
        canonical_value(ComponentId::BlockInfo, &[], &pre_ironwood).expect("valid"),
        block_info(&pools[..40]),
    );

    // Trailing bytes are ignored, as the state ignores them.
    let mut trailing = current.clone();
    trailing.push(0xFF);
    assert_eq!(
        canonical_value(ComponentId::BlockInfo, &[], &trailing).expect("valid"),
        current,
    );

    assert_err!(
        canonical_value(ComponentId::BlockInfo, &[], &current[..43]),
        FormatError::InvalidValue {
            component: ComponentId::BlockInfo,
            ..
        },
    );
    let mut negative_pool = current.clone();
    negative_pool[..8].copy_from_slice(&(-5_i64).to_le_bytes());
    assert_err!(
        canonical_value(ComponentId::BlockInfo, &[], &negative_pool),
        FormatError::InvalidValue { .. },
    );
    let mut negative_nsm = current.clone();
    negative_nsm[52..60].copy_from_slice(&(-5_i64).to_le_bytes());
    assert_err!(
        canonical_value(ComponentId::BlockInfo, &[], &negative_nsm),
        FormatError::InvalidValue { .. },
    );
}

#[test]
fn address_balances_gain_a_received_total() {
    let _init_guard = zebra_test::init();

    // The balance, the address location (height 1, transaction 0, output 1), and the total.
    let mut current = 50_000_i64.to_le_bytes().to_vec();
    current.extend_from_slice(&[0, 0, 1, 0, 0, 0, 0, 1]);
    current.extend_from_slice(&70_000_u64.to_le_bytes());
    assert_eq!(
        canonical_value(ComponentId::AddressBalances, &[], &current).expect("valid"),
        current,
    );

    // Before the `received` total, it reads as zero.
    let mut expected = current[..16].to_vec();
    expected.extend_from_slice(&0_u64.to_le_bytes());
    assert_eq!(
        canonical_value(ComponentId::AddressBalances, &[], &current[..16]).expect("valid"),
        expected,
    );

    assert_err!(
        canonical_value(ComponentId::AddressBalances, &[], &current[..15]),
        FormatError::InvalidValue {
            component: ComponentId::AddressBalances,
            ..
        },
    );
    let mut negative = current.clone();
    negative[..8].copy_from_slice(&(-1_i64).to_le_bytes());
    assert_err!(
        canonical_value(ComponentId::AddressBalances, &[], &negative),
        FormatError::InvalidValue { .. },
    );
}

/// The layout of a history tree written before NU6.3, with narrower entries.
#[derive(serde::Serialize)]
struct LegacyParts {
    network_kind: NetworkKind,
    size: u32,
    peaks: BTreeMap<u32, LegacyEntry>,
    current_height: Height,
}

/// A history tree entry written before NU6.3.
#[derive(serde::Serialize)]
struct LegacyEntry {
    #[serde(with = "BigArray")]
    inner: [u8; 253],
}

#[test]
fn history_trees_widen_legacy_entries() {
    let _init_guard = zebra_test::init();

    // A two-block Heartwood tree from the Mainnet vectors.
    let network = Network::Mainnet;
    let (blocks, sapling_roots) = network.block_sapling_roots_map();
    let block = |height: u32| -> Arc<Block> {
        blocks
            .get(&height)
            .expect("vector exists")
            .zcash_deserialize_into()
            .expect("valid block")
    };
    let root = |height: u32| {
        sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("vector exists"))
            .expect("valid root")
    };
    let mut tree = NonEmptyHistoryTree::from_block(
        &network,
        block(903_000),
        BlockCommitmentTreeRoots {
            sapling: &root(903_000),
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .expect("valid tree");
    tree.push(
        block(903_001),
        BlockCommitmentTreeRoots {
            sapling: &root(903_001),
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .expect("valid tree");

    let current = HistoryTreeParts::from(&tree).as_bytes();
    assert_eq!(
        canonical_value(ComponentId::HistoryTree, &[], &current).expect("valid"),
        current,
    );

    // The same tree with entries in the pre-NU6.3 width: their node data ends before the extra
    // bytes, which are zero.
    let options = bincode::DefaultOptions::new();
    let legacy = LegacyParts {
        network_kind: network.kind(),
        size: tree.size(),
        peaks: tree
            .peaks()
            .iter()
            .map(|(index, entry)| {
                let bytes = options.serialize(entry).expect("serializes");
                assert!(bytes[253..].iter().all(|byte| *byte == 0));
                let inner = bytes[..253].try_into().expect("253 bytes");
                (*index, LegacyEntry { inner })
            })
            .collect(),
        current_height: tree.current_height(),
    };
    let legacy = options.serialize(&legacy).expect("serializes");
    assert_ne!(legacy, current);
    assert_eq!(
        canonical_value(ComponentId::HistoryTree, &[], &legacy).expect("valid"),
        current,
    );

    for invalid in [
        &current[..current.len() - 1],
        &legacy[..legacy.len() - 1],
        b"tree",
    ] {
        assert_err!(
            canonical_value(ComponentId::HistoryTree, &[], invalid),
            FormatError::InvalidValue {
                component: ComponentId::HistoryTree,
                ..
            },
        );
    }
}

/// Replaces the cached root at the end of the encoding of a tree that has
/// one with `root`.
fn with_cached_root(encoded: &[u8], root: [u8; 32]) -> Vec<u8> {
    let (tree, cached) = encoded.split_at(encoded.len() - 33);
    assert_eq!(cached[0], 1, "the tree has a cached root");
    [tree, &[1], &root].concat()
}

#[test]
fn trees_carry_their_recomputed_cached_root() {
    let _init_guard = zebra_test::init();

    // The empty tree has no cached root until its root is computed.
    let sapling_tree = sapling::tree::NoteCommitmentTree::default();
    let without_root = sapling_tree.as_bytes();
    sapling_tree.root();
    let with_root = sapling_tree.as_bytes();
    assert_ne!(without_root, with_root);
    for value in [&without_root, &with_root] {
        assert_eq!(
            canonical_value(ComponentId::SaplingTree, &[], value).expect("valid"),
            with_root,
        );
    }

    // A wrong cached root is refused: the state would trust it.
    let wrong: [u8; 32] = **Network::Mainnet
        .block_sapling_roots_map()
        .1
        .get(&419_201)
        .expect("vector exists");
    let wrong_root = with_cached_root(&with_root, wrong);
    let decoded: sapling::tree::NoteCommitmentTree = bincode::DefaultOptions::new()
        .deserialize(&wrong_root)
        .expect("a valid tree");
    assert_eq!(
        decoded.cached_root(),
        Some(sapling::tree::Root::try_from(wrong).expect("valid root"))
    );
    assert_err!(
        canonical_value(ComponentId::SaplingTree, &[], &wrong_root),
        FormatError::WrongCachedRoot {
            component: ComponentId::SaplingTree
        },
    );

    // Orchard and Ironwood use the same tree.
    let orchard_tree = orchard::tree::NoteCommitmentTree::default();
    let without_root = orchard_tree.as_bytes();
    orchard_tree.root();
    let with_root = orchard_tree.as_bytes();
    // The first Orchard actions are at 1,687,107, so its root is not the empty tree's.
    let mut wrong = **Network::Mainnet
        .orchard_anchors()
        .get(&1_687_107)
        .expect("vector exists");
    // The Orchard vectors are stored reversed from their serialized order.
    wrong.reverse();
    assert_ne!(
        orchard::tree::Root::try_from(wrong).expect("valid root"),
        orchard_tree.root(),
    );
    let wrong_root = with_cached_root(&with_root, wrong);
    for component in [ComponentId::OrchardTree, ComponentId::IronwoodTree] {
        for value in [&without_root, &with_root] {
            assert_eq!(
                canonical_value(component, &[], value).expect("valid"),
                with_root,
            );
        }
        assert_err!(
            canonical_value(component, &[], &wrong_root),
            FormatError::WrongCachedRoot { component: found } if found == component,
        );
    }

    // A Sprout tree with a note, at the tip and as an anchor keyed by its root.
    let mut sprout_tree = sprout::tree::NoteCommitmentTree::default();
    sprout_tree
        .append(sprout::NoteCommitment::from([7; 32]))
        .expect("the tree has room");
    let without_root = sprout_tree.as_bytes();
    let root = sprout_tree.root();
    let with_root = sprout_tree.as_bytes();
    assert_ne!(root, sprout::tree::NoteCommitmentTree::default().root());
    for value in [&without_root, &with_root] {
        assert_eq!(
            canonical_value(ComponentId::SproutTipTree, &[], value).expect("valid"),
            with_root,
        );
        assert_eq!(
            canonical_value(ComponentId::SproutAnchors, &root.as_bytes(), value).expect("valid"),
            with_root,
        );
    }
    let wrong_root = with_cached_root(&with_root, [0x42; 32]);
    for component in [ComponentId::SproutTipTree, ComponentId::SproutAnchors] {
        assert_err!(
            canonical_value(component, &root.as_bytes(), &wrong_root),
            FormatError::WrongCachedRoot { component: found } if found == component,
        );
    }

    // An anchor is the root of its tree.
    assert_err!(
        canonical_value(ComponentId::SproutAnchors, &[0x42; 32], &with_root),
        FormatError::InvalidValue {
            component: ComponentId::SproutAnchors,
            ..
        },
    );

    for component in [
        ComponentId::SaplingTree,
        ComponentId::OrchardTree,
        ComponentId::IronwoodTree,
        ComponentId::SproutTipTree,
        ComponentId::SproutAnchors,
    ] {
        assert_err!(
            canonical_value(component, &[], &[0xFF; 7]),
            FormatError::InvalidValue { component: found, .. } if found == component,
        );
    }
}

/// The last leaf of a hand-assembled frontier, with its left sibling when it
/// is a right child.
enum Leaf {
    Left([u8; 32]),
    Right([u8; 32], [u8; 32]),
}

/// Returns the on-disk encoding of a note commitment tree whose frontier is at
/// `position` with `leaf` and `ommers`, and with `cached_root`, assembled by
/// hand from the legacy layout the trees serialize through: an `Option`
/// tag, the position, the leaf's variant and nodes, the ommers with their
/// count, and the cached root's `Option` tag, with every integer as bincode's
/// default varint.
fn tree_bytes(
    position: u64,
    leaf: Leaf,
    ommers: &[[u8; 32]],
    cached_root: Option<[u8; 32]>,
) -> Vec<u8> {
    let varint = |value: u64| {
        bincode::DefaultOptions::new()
            .serialize(&value)
            .expect("serializes")
    };

    let mut bytes = vec![1];
    bytes.extend(varint(position));
    match leaf {
        Leaf::Left(leaf) => {
            bytes.extend(varint(0));
            bytes.extend(leaf);
        }
        Leaf::Right(left, leaf) => {
            bytes.extend(varint(1));
            bytes.extend(left);
            bytes.extend(leaf);
        }
    }
    bytes.extend(varint(u64::try_from(ommers.len()).expect("a few ommers")));
    for ommer in ommers {
        bytes.extend(ommer);
    }
    match cached_root {
        Some(root) => {
            bytes.push(1);
            bytes.extend(root);
        }
        None => bytes.push(0),
    }
    bytes
}

/// The state's `FromDisk` panics on a tree whose frontier does not fit its
/// position or its depth, and on Sapling and Orchard nodes that are not
/// canonical field elements. The canonical form refuses them instead, so the
/// importer can refuse a piece.
#[test]
fn trees_the_state_would_panic_on_are_refused() {
    let _init_guard = zebra_test::init();

    // `tree_bytes` assembles the trees' layout: a Sprout tree with one note, then two, then with
    // its cached root.
    let mut sprout_tree = sprout::tree::NoteCommitmentTree::default();
    sprout_tree
        .append(sprout::NoteCommitment::from([7; 32]))
        .expect("the tree has room");
    assert_eq!(
        sprout_tree.as_bytes(),
        tree_bytes(0, Leaf::Left([7; 32]), &[], None)
    );
    sprout_tree
        .append(sprout::NoteCommitment::from([8; 32]))
        .expect("the tree has room");
    assert_eq!(
        sprout_tree.as_bytes(),
        tree_bytes(1, Leaf::Right([7; 32], [8; 32]), &[], None)
    );
    let root: [u8; 32] = sprout_tree.root().into();
    assert_eq!(
        sprout_tree.as_bytes(),
        tree_bytes(1, Leaf::Right([7; 32], [8; 32]), &[], Some(root))
    );

    // Zero is a canonical field element in every pool.
    let node = [0; 32];
    for (component, depth) in [
        (ComponentId::SaplingTree, 32),
        (ComponentId::OrchardTree, 32),
        (ComponentId::IronwoodTree, 32),
        (ComponentId::SproutTipTree, 29),
    ] {
        let canonical = |bytes: &[u8]| canonical_value(component, &[], bytes);

        // Frontiers whose ommers fit their position decode: one leaf, two, and a full tree, whose
        // position has `depth` set bits.
        canonical(&tree_bytes(0, Leaf::Left(node), &[], None)).expect("valid");
        canonical(&tree_bytes(1, Leaf::Right(node, node), &[], None)).expect("valid");
        canonical(&tree_bytes(2, Leaf::Left(node), &[node], None)).expect("valid");
        let full = (1_u64 << depth) - 1;
        canonical(&tree_bytes(
            full,
            Leaf::Right(node, node),
            &vec![node; depth - 1],
            None,
        ))
        .expect("valid");

        // `Frontier::from_parts` needs one ommer per set bit of the position, counting a right
        // leaf's sibling, and the state unwraps its result.
        for (position, leaf, ommers) in [
            (0, Leaf::Left(node), 7),
            (0, Leaf::Right(node, node), 0),
            (1, Leaf::Left(node), 0),
            (1, Leaf::Right(node, node), 1),
            (3, Leaf::Right(node, node), 0),
            (full, Leaf::Right(node, node), depth),
        ] {
            assert_err!(
                canonical(&tree_bytes(position, leaf, &vec![node; ommers], None)),
                FormatError::InvalidValue { component: found, .. } if found == component,
            );
        }

        // A position past the tree's depth.
        assert_err!(
            canonical(&tree_bytes(1 << depth, Leaf::Left(node), &[node], None)),
            FormatError::InvalidValue { component: found, .. } if found == component,
        );
    }

    // Sapling nodes and roots are `jubjub::Base` and Orchard's `pallas::Base`, whose serde
    // helpers unwrap non-canonical encodings. Every byte set is above both moduli.
    let non_canonical = [0xFF; 32];
    for component in [
        ComponentId::SaplingTree,
        ComponentId::OrchardTree,
        ComponentId::IronwoodTree,
    ] {
        for bytes in [
            tree_bytes(0, Leaf::Left(non_canonical), &[], None),
            tree_bytes(1, Leaf::Right(non_canonical, node), &[], None),
            tree_bytes(1, Leaf::Right(node, non_canonical), &[], None),
            tree_bytes(2, Leaf::Left(node), &[non_canonical], None),
            tree_bytes(0, Leaf::Left(node), &[], Some(non_canonical)),
        ] {
            assert_err!(
                canonical_value(component, &[], &bytes),
                FormatError::InvalidValue { component: found, .. } if found == component,
            );
        }
    }

    // Sprout nodes are any 32 bytes.
    canonical_value(
        ComponentId::SproutTipTree,
        &[],
        &tree_bytes(0, Leaf::Left(non_canonical), &[], None),
    )
    .expect("valid");
}

#[test]
fn other_components_are_copied_unchanged() {
    let _init_guard = zebra_test::init();

    for component in ComponentId::ALL {
        if component.is_canonicalized() {
            continue;
        }
        for value in [&b""[..], b"\x00", &[0xAB; 300]] {
            assert_eq!(
                canonical_value(component, b"key", value).expect("copied"),
                value,
                "{component}",
            );
        }
    }

    // Every canonicalized component checks its values.
    for component in ComponentId::ALL {
        if component.is_canonicalized() {
            assert!(
                canonical_value(component, &[], &[0xFF; 7]).is_err(),
                "{component}"
            );
        }
    }
}
