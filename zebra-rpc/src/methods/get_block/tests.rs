//! Tests for the `getblock` field derivations over Mainnet blocks from each network upgrade.
//!
//! The RPC snapshots only cover heights 0 to 10, where `blockcommitments` and `finalsaplingroot`
//! are zeros and the trees are empty, so these tests pin the derivations that only later blocks
//! exercise.

use std::sync::Arc;

use hex::ToHex;
use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Block, Commitment, Height},
    block_info::BlockInfo,
    orchard,
    parameters::Network::{self, Mainnet},
    sapling,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    value_balance::ValueBalance,
};

use super::*;

/// Mainnet heights just before Sapling, at and after the Sapling, Blossom, Heartwood, Canopy and
/// NU5 activations.
const UPGRADE_HEIGHTS: [u32; 11] = [
    419_199, 419_200, 419_201, 419_202, 653_600, 903_000, 903_001, 1_046_400, 1_687_106, 1_687_107,
    1_687_108,
];

/// The byte range of the commitment field in a serialized block header.
const COMMITMENT_BYTES: std::ops::Range<usize> = 68..100;

/// The byte range of the nonce field in a serialized block header.
const NONCE_BYTES: std::ops::Range<usize> = 108..140;

fn mainnet_block(height: u32) -> Arc<Block> {
    zebra_test::vectors::MAINNET_BLOCKS[&height]
        .zcash_deserialize_into()
        .expect("the test vectors are valid blocks")
}

fn header_bytes(block: &Block) -> Vec<u8> {
    block
        .header
        .zcash_serialize_to_vec()
        .expect("writing to a vector never fails")
}

fn reversed(bytes: &[u8]) -> [u8; 32] {
    let mut bytes: [u8; 32] = bytes.try_into().expect("the caller passes 32 bytes");
    bytes.reverse();
    bytes
}

/// A Sapling tree holding the note commitments of the first Mainnet vector block that has any,
/// twice, so its root is not the root of any Mainnet block.
fn non_empty_sapling_tree() -> sapling::tree::NoteCommitmentTree {
    let mut tree = sapling::tree::NoteCommitmentTree::default();
    let block = zebra_test::vectors::MAINNET_BLOCKS
        .values()
        .map(|bytes| {
            bytes
                .zcash_deserialize_into::<Block>()
                .expect("the test vectors are valid blocks")
        })
        .find(|block| block.sapling_note_commitments().next().is_some())
        .expect("the Mainnet vectors contain Sapling outputs");
    for commitment in block
        .sapling_note_commitments()
        .chain(block.sapling_note_commitments())
    {
        tree.append(commitment)
            .expect("two blocks' outputs fit in the tree");
    }
    tree
}

/// A non-empty Orchard tree.
fn non_empty_orchard_tree() -> orchard::tree::NoteCommitmentTree {
    let mut tree = orchard::tree::NoteCommitmentTree::default();
    tree.append(orchard::tree::NoteCommitmentUpdate::from(1))
        .expect("an empty tree has room for one commitment");
    tree
}

#[test]
fn nonce_is_the_header_nonce_reversed() {
    let _init_guard = zebra_test::init();

    for height in UPGRADE_HEIGHTS {
        let block = mainnet_block(height);
        assert_eq!(
            nonce(&block.header),
            reversed(&header_bytes(&block)[NONCE_BYTES]),
            "height {height}",
        );
    }
}

#[test]
fn final_sapling_root_is_zero_before_sapling_and_the_reversed_tree_root_after() {
    let _init_guard = zebra_test::init();

    let empty = sapling::tree::NoteCommitmentTree::default();
    let notes = non_empty_sapling_tree();
    assert_ne!(empty.root(), notes.root());

    for height in UPGRADE_HEIGHTS {
        let height = Height(height);
        for tree in [&empty, &notes] {
            let expected = if height >= Height(419_200) {
                let root: [u8; 32] = tree.root().into();
                reversed(&root)
            } else {
                [0; 32]
            };
            assert_eq!(
                final_sapling_root(&Mainnet, height, tree),
                expected,
                "{height:?}"
            );
        }
    }
}

#[test]
fn block_commitments_follow_each_network_upgrade() {
    let _init_guard = zebra_test::init();

    let notes = non_empty_sapling_tree();

    for height in UPGRADE_HEIGHTS {
        let block = mainnet_block(height);
        let header_commitment = &header_bytes(&block)[COMMITMENT_BYTES];
        let height = Height(height);
        let final_sapling_root = final_sapling_root(&Mainnet, height, &notes);
        let block_commitments =
            block_commitments(&block.header, &Mainnet, height, final_sapling_root);

        match block
            .header
            .commitment(&Mainnet, height)
            .expect("the test vectors have valid commitments")
        {
            Commitment::PreSaplingReserved(_) => {
                assert!(height < Height(419_200));
                assert_eq!(block_commitments, header_commitment, "{height:?}");
            }
            // The Sapling and Blossom arm is the tree root, not the header bytes, because the
            // state does not check that they are equal.
            Commitment::FinalSaplingRoot(_) => {
                assert!(height >= Height(419_200) && height < Height(903_000));
                assert_eq!(block_commitments, final_sapling_root, "{height:?}");
                assert_ne!(
                    block_commitments,
                    reversed(header_commitment),
                    "{height:?}: the tree root is not the header's root"
                );
            }
            Commitment::ChainHistoryActivationReserved => {
                assert_eq!(height, Height(903_000));
                assert_eq!(block_commitments, [0; 32]);
            }
            Commitment::ChainHistoryRoot(_) => {
                assert!(height > Height(903_000) && height < Height(1_687_104));
                assert_eq!(block_commitments, reversed(header_commitment), "{height:?}");
            }
            Commitment::ChainHistoryBlockTxAuthCommitment(_) => {
                assert!(height >= Height(1_687_104));
                assert_eq!(block_commitments, reversed(header_commitment), "{height:?}");
            }
        }
    }
}

#[test]
fn sapling_activation_header_holds_the_empty_tree_root() {
    let _init_guard = zebra_test::init();

    // At the Sapling activation block the empty-tree derivation and the header agree, which is
    // the case where a header-only `blockcommitments` would happen to match.
    let block = mainnet_block(419_200);
    let height = Height(419_200);
    let empty = sapling::tree::NoteCommitmentTree::default();
    let final_sapling_root = final_sapling_root(&Mainnet, height, &empty);

    assert_eq!(
        final_sapling_root,
        reversed(&header_bytes(&block)[COMMITMENT_BYTES])
    );
    assert_eq!(
        block_commitments(&block.header, &Mainnet, height, final_sapling_root),
        final_sapling_root,
    );
}

#[test]
fn final_orchard_root_is_omitted_before_nu5() {
    let _init_guard = zebra_test::init();

    let tree = non_empty_orchard_tree();
    let root: [u8; 32] = tree.root().into();

    for height in UPGRADE_HEIGHTS {
        let expected = (height >= 1_687_104).then_some(root);
        assert_eq!(
            final_orchard_root(&Mainnet, Height(height), &tree),
            expected,
            "height {height}"
        );
    }
}

#[test]
fn confirmations_are_one_more_than_the_depth() {
    assert_eq!(confirmations(Some(0)), 1);
    assert_eq!(confirmations(Some(9)), 10);
    assert_eq!(confirmations(Some(u32::MAX)), i64::from(u32::MAX) + 1);
    assert_eq!(confirmations(None), -1);
}

#[test]
fn trees_omit_empty_pools() {
    for (sizes, expected) in [
        ((0, 0, 0), serde_json::json!({})),
        ((5, 0, 0), serde_json::json!({ "sapling": { "size": 5 } })),
        (
            (0, 3, 2),
            serde_json::json!({ "orchard": { "size": 3 }, "ironwood": { "size": 2 } }),
        ),
    ] {
        let (sapling, orchard, ironwood) = sizes;
        assert_eq!(
            serde_json::to_value(trees(sapling, orchard, ironwood)).unwrap(),
            expected
        );
    }
}

fn block_info(transparent: i64, size: u32) -> BlockInfo {
    let transparent = Amount::<NonNegative>::try_from(transparent).expect("the value is valid");
    BlockInfo::new(ValueBalance::from_transparent_amount(transparent), size)
}

#[test]
fn value_pools_and_size_use_block_info() {
    let _init_guard = zebra_test::init();

    let parent = block_info(100, 10);
    let current = block_info(150, 20);

    let delta = value_pool_delta(Some(&current), Some(parent.clone())).expect("the delta fits");
    assert_eq!(delta.transparent_amount().zatoshis(), 50);

    // A missing parent row is a zero parent balance.
    let delta = value_pool_delta(Some(&current), None).expect("the delta fits");
    assert_eq!(delta.transparent_amount().zatoshis(), 150);

    assert_eq!(value_pool_delta(None, Some(parent)), None);

    // The body's size wins over the cached size.
    assert_eq!(block_size(Some(7), Some(&current)), Some(7));
    assert_eq!(block_size(None, Some(&current)), Some(20));
    assert_eq!(block_size(None, None), None);

    assert_eq!(chain_supply(None), None);
    assert_eq!(value_pools(None, None), None);
    let pools = value_pools(Some(current.clone()), Some(delta)).expect("block info is present");
    let pools = serde_json::to_value(pools).unwrap();
    assert_eq!(pools[0]["id"], "transparent");
    assert_eq!(pools[0]["chainValueZat"], 150);
    assert_eq!(pools[0]["valueDeltaZat"], 150);
}

/// Pins the derived values for each upgrade's blocks, as the getblock handlers computed them
/// before the field derivations moved here.
#[test]
fn field_derivations_snapshot() {
    let _init_guard = zebra_test::init();

    let notes = non_empty_sapling_tree();
    let network = Network::Mainnet;

    let rows: Vec<_> = UPGRADE_HEIGHTS
        .into_iter()
        .map(|height| {
            let block = mainnet_block(height);
            let height = Height(height);
            let empty_tree_root = final_sapling_root(
                &network,
                height,
                &sapling::tree::NoteCommitmentTree::default(),
            );
            let notes_root = final_sapling_root(&network, height, &notes);
            serde_json::json!({
                "height": height.0,
                "nonce": hex::encode(nonce(&block.header)),
                "bits": block.header.difficulty_threshold.encode_hex::<String>(),
                "difficulty": difficulty(&block.header, &network),
                "finalsaplingroot_empty_tree": hex::encode(empty_tree_root),
                "finalsaplingroot_note_tree": hex::encode(notes_root),
                "blockcommitments_empty_tree": hex::encode(block_commitments(
                    &block.header,
                    &network,
                    height,
                    empty_tree_root,
                )),
                "blockcommitments_note_tree": hex::encode(block_commitments(
                    &block.header,
                    &network,
                    height,
                    notes_root,
                )),
            })
        })
        .collect();

    // zebrad's dev-dependencies enable `serde_json/preserve_order`, so sort the maps to keep the
    // snapshot independent of the packages a test run selects.
    insta::with_settings!({ sort_maps => true }, {
        insta::assert_json_snapshot!("field_derivations@mainnet", rows);
    });
}
