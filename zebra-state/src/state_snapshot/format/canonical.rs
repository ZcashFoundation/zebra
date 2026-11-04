//! The canonical values of the components whose rows have more than one
//! on-disk encoding (design, "Snapshot contents").
//!
//! A database keeps the encoding each row was written in: value pools and
//! block info written before newer pools were added are narrower, history
//! trees written before NU6.3 have narrower entries, address balances written
//! before the `received` total have no total, and note commitment trees may or
//! may not carry their cached root. Each such value is decoded with the
//! database's `FromDisk` and re-encoded with the current `IntoDisk`, so two
//! databases with the same chain state export the same bytes, whatever their
//! upgrade history. Every other component has a single encoding, so its values
//! are copied as they are.

use bincode::Options;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block_info::BlockInfo,
    orchard, sapling, sprout,
    value_balance::ValueBalance,
};

use crate::service::finalized_state::{
    AddressBalanceLocation, FromDisk, HistoryTreeParts, IntoDisk, LegacyHistoryTreeParts,
    OutputLocation,
};

use super::{ComponentId, FormatError};

/// Returns the canonical encoding of `value`, the value of a row of
/// `component` with the key `key`.
///
/// For a component that [`ComponentId::is_canonicalized`], the value is
/// decoded and re-encoded in the current database format. Note commitment
/// trees always carry their cached root: a tree without one gets its
/// recomputed root, and a tree whose cached root is not its root is refused
/// with [`FormatError::WrongCachedRoot`], because the state would trust that
/// root. A Sprout anchor's tree must also have the anchor as its root. Every
/// other component's value is returned unchanged.
///
/// Values that do not decode are refused with [`FormatError::InvalidValue`],
/// including the values the state's `FromDisk` would panic on, so the
/// importer can refuse a piece whose records are not what the manifest
/// vouched for.
pub fn canonical_value(
    component: ComponentId,
    key: &[u8],
    value: &[u8],
) -> Result<Vec<u8>, FormatError> {
    match component {
        ComponentId::BlockInfo => block_info(value),
        ComponentId::ValuePools => value_pools(value),
        ComponentId::HistoryTree => history_tree(value),
        ComponentId::AddressBalances => address_balance(value),
        ComponentId::SaplingTree => sapling_tree(value),
        ComponentId::OrchardTree | ComponentId::IronwoodTree => orchard_tree(component, value),
        ComponentId::SproutTipTree => Ok(sprout_tree(component, value)?.0),
        ComponentId::SproutAnchors => {
            let (canonical, root) = sprout_tree(component, value)?;
            if key != root.as_bytes() {
                return Err(invalid(
                    component,
                    "the Sprout tree's root is not its anchor",
                ));
            }
            Ok(canonical)
        }

        ComponentId::HashByHeight
        | ComponentId::HeightByHash
        | ComponentId::Headers
        | ComponentId::Transactions
        | ComponentId::HashByTxLoc
        | ComponentId::TxLocByHash
        | ComponentId::Utxos
        | ComponentId::AddressUtxos
        | ComponentId::AddressTxs
        | ComponentId::SproutNullifiers
        | ComponentId::SaplingNullifiers
        | ComponentId::OrchardNullifiers
        | ComponentId::IronwoodNullifiers
        | ComponentId::SaplingAnchors
        | ComponentId::OrchardAnchors
        | ComponentId::IronwoodAnchors
        | ComponentId::SaplingSubtrees
        | ComponentId::OrchardSubtrees
        | ComponentId::IronwoodSubtrees => Ok(value.to_vec()),
    }
}

/// Returns the error for a `component` value that does not decode.
fn invalid(component: ComponentId, reason: &'static str) -> FormatError {
    FormatError::InvalidValue { component, reason }
}

/// Re-encodes a `block_info` value, which is 44 bytes before NU6.3, 52 bytes
/// in format 28, and 60 bytes since the NSM pool.
fn block_info(value: &[u8]) -> Result<Vec<u8>, FormatError> {
    let component = ComponentId::BlockInfo;

    // `FromDisk` reads the pools before the block size, and the NSM pool after it in the widest
    // layout, and panics on invalid amounts, so they are checked first.
    let pools_len = match value.len() {
        52.. => 48,
        44.. => 40,
        _ => return Err(invalid(component, "the value is shorter than any layout")),
    };
    ValueBalance::<NonNegative>::from_bytes(&value[..pools_len])
        .map_err(|_| invalid(component, "the value pools are not valid amounts"))?;
    if let Some(nsm) = value.get(52..60) {
        let nsm = nsm
            .try_into()
            .map_err(|_| invalid(component, "the NSM pool is not 8 bytes"))?;
        Amount::<NonNegative>::from_bytes(nsm)
            .map_err(|_| invalid(component, "the NSM pool is not a valid amount"))?;
    }

    Ok(BlockInfo::from_bytes(value).as_bytes().to_vec())
}

/// Re-encodes a `tip_chain_value_pool` value, which is 32, 40, 48 or 56 bytes
/// as pools were added.
fn value_pools(value: &[u8]) -> Result<Vec<u8>, FormatError> {
    // This is the value pools' `FromDisk` without its panic, and their `IntoDisk`.
    let pools = ValueBalance::<NonNegative>::from_bytes(value).map_err(|_| {
        invalid(
            ComponentId::ValuePools,
            "the value is not a value balance in any layout",
        )
    })?;

    Ok(pools.as_bytes().to_vec())
}

/// Re-encodes a `balance_by_transparent_addr` value, with or without its `received` total.
fn address_balance(value: &[u8]) -> Result<Vec<u8>, FormatError> {
    let component = ComponentId::AddressBalances;

    // Check the fallible fields before `FromDisk`, then let it supply a zero for a missing total.
    let (balance, rest) = value
        .split_at_checked(8)
        .ok_or_else(|| invalid(component, "the value is shorter than a balance"))?;
    let (_, received) = OutputLocation::from_bytes_prefix(rest)
        .ok_or_else(|| invalid(component, "the address location is not canonical"))?;
    if !matches!(received.len(), 0 | 8) {
        return Err(invalid(component, "the received total is not 8 bytes"));
    }
    Amount::<NonNegative>::from_bytes(
        balance
            .try_into()
            .map_err(|_| invalid(component, "the balance is not 8 bytes"))?,
    )
    .map_err(|_| invalid(component, "the balance is not a valid amount"))?;

    Ok(AddressBalanceLocation::from_bytes(value)
        .as_bytes()
        .to_vec())
}

/// Re-encodes a `history_tree` value, whose entries were narrower before
/// NU6.3.
fn history_tree(value: &[u8]) -> Result<Vec<u8>, FormatError> {
    let options = bincode::DefaultOptions::new();

    // This is `HistoryTreeParts`' `FromDisk` without its panic: the current layout, then the
    // legacy layout it falls back to.
    let parts = options
        .deserialize::<HistoryTreeParts>(value)
        .or_else(|_| {
            options
                .deserialize::<LegacyHistoryTreeParts>(value)
                .map(HistoryTreeParts::from)
        })
        .map_err(|_| {
            invalid(
                ComponentId::HistoryTree,
                "the value is not a history tree in the current or the legacy layout",
            )
        })?;

    Ok(parts.as_bytes())
}

/// Re-encodes a Sapling note commitment tree with its recomputed root.
fn sapling_tree(value: &[u8]) -> Result<Vec<u8>, FormatError> {
    let component = ComponentId::SaplingTree;
    let tree: sapling::tree::NoteCommitmentTree = decode_tree(
        component,
        value,
        SAPLING_MERKLE_DEPTH,
        sapling_node_is_valid,
    )?;

    match tree.cached_root() {
        Some(cached) if cached != tree.recalculate_root() => {
            return Err(FormatError::WrongCachedRoot { component })
        }
        Some(_correct) => {}
        // Computing the root caches it.
        None => {
            tree.root();
        }
    }

    Ok(tree.as_bytes())
}

/// Re-encodes an Orchard or Ironwood note commitment tree with its recomputed
/// root: both pools use the Orchard tree.
fn orchard_tree(component: ComponentId, value: &[u8]) -> Result<Vec<u8>, FormatError> {
    let tree: orchard::tree::NoteCommitmentTree = decode_tree(
        component,
        value,
        SAPLING_MERKLE_DEPTH,
        orchard_node_is_valid,
    )?;

    match tree.cached_root() {
        Some(cached) if cached != tree.recalculate_root() => {
            return Err(FormatError::WrongCachedRoot { component })
        }
        Some(_correct) => {}
        // Computing the root caches it.
        None => {
            tree.root();
        }
    }

    Ok(tree.as_bytes())
}

/// Re-encodes a Sprout note commitment tree with its recomputed root, and
/// returns the root.
fn sprout_tree(
    component: ComponentId,
    value: &[u8],
) -> Result<(Vec<u8>, sprout::tree::Root), FormatError> {
    let tree: sprout::tree::NoteCommitmentTree =
        decode_tree(component, value, SPROUT_MERKLE_DEPTH, sprout_node_is_valid)?;

    let root = match tree.cached_root() {
        Some(cached) => {
            let root = tree.recalculate_root();
            if cached != root {
                return Err(FormatError::WrongCachedRoot { component });
            }
            root
        }
        // Computing the root caches it.
        None => tree.root(),
    };

    Ok((tree.as_bytes(), root))
}

/// The depth of the Sapling note commitment tree, which the Orchard and
/// Ironwood trees share: `zebra_chain::sapling::tree::MERKLE_DEPTH`, which is
/// not public.
const SAPLING_MERKLE_DEPTH: u32 = 32;

/// The depth of the Sprout note commitment tree:
/// `zebra_chain::sprout::tree::MERKLE_DEPTH`, which is not public.
const SPROUT_MERKLE_DEPTH: u32 = 29;

/// The on-disk layout of a note commitment tree: the legacy types the trees
/// serialize through, with every node and root as its 32 bytes.
///
/// A tree is parsed into this layout before it is decoded, to refuse the
/// values whose conversion from the legacy types panics: a frontier whose
/// ommers do not fit its position or whose position is too deep for the tree
/// (`Frontier::from_parts`), and nodes or roots that are not canonical field
/// elements (the serde helpers unwrap them). The field types and order match
/// the legacy types, which bincode identifies by position.
#[derive(serde::Deserialize)]
struct TreeLayout {
    inner: FrontierLayout,
    cached_root: Option<[u8; 32]>,
}

/// The layout of a tree's `Frontier`.
#[derive(serde::Deserialize)]
struct FrontierLayout {
    frontier: Option<NonEmptyFrontierLayout>,
}

/// The layout of a `NonEmptyFrontier`.
#[derive(serde::Deserialize)]
struct NonEmptyFrontierLayout {
    /// The position of the last leaf, a `usize` on disk, which bincode
    /// encodes as it encodes a `u64`.
    position: u64,
    leaf: LeafLayout,
    ommers: Vec<[u8; 32]>,
}

/// The layout of a frontier's `Leaf`: the last leaf, with its left sibling
/// when it is a right child.
#[derive(serde::Deserialize)]
enum LeafLayout {
    Left([u8; 32]),
    Right([u8; 32], [u8; 32]),
}

/// Whether `node` is a Sapling node or root: a canonical `jubjub::Base`.
fn sapling_node_is_valid(node: &[u8; 32]) -> bool {
    sapling_crypto::Node::from_bytes(*node).is_some().into()
}

/// Whether `node` is an Orchard node or root: a canonical `pallas::Base`.
fn orchard_node_is_valid(node: &[u8; 32]) -> bool {
    orchard::tree::Node::try_from(*node).is_ok()
}

/// Whether `node` is a Sprout node or root: any 32 bytes.
fn sprout_node_is_valid(_node: &[u8; 32]) -> bool {
    true
}

/// Decodes a note commitment tree of `depth` as the state's `FromDisk` does,
/// returning an error instead of panicking if it is not a tree, or if it is a
/// tree the conversion from the on-disk layout panics on: see [`TreeLayout`].
/// `node_is_valid` tells whether 32 bytes are a node or root of the tree.
fn decode_tree<T: serde::de::DeserializeOwned>(
    component: ComponentId,
    value: &[u8],
    depth: u32,
    node_is_valid: fn(&[u8; 32]) -> bool,
) -> Result<T, FormatError> {
    let options = bincode::DefaultOptions::new();
    let layout: TreeLayout = options
        .deserialize(value)
        .map_err(|_| invalid(component, "the value is not a note commitment tree"))?;

    let mut nodes: Vec<&[u8; 32]> = layout.cached_root.iter().collect();
    if let Some(frontier) = &layout.inner.frontier {
        // A tree of `depth` levels holds at most `2^depth` leaves.
        if frontier.position >> depth != 0 {
            return Err(invalid(
                component,
                "the frontier's position is deeper than the tree",
            ));
        }

        // `Frontier::from_parts` requires one ommer per set bit of the position (every set bit
        // is below the position's root level, so `past_ommer_count` is the number of set bits),
        // and a right leaf's left sibling is the first of them.
        let sibling = match &frontier.leaf {
            LeafLayout::Left(leaf) => {
                nodes.push(leaf);
                0
            }
            LeafLayout::Right(left, leaf) => {
                nodes.push(left);
                nodes.push(leaf);
                1
            }
        };
        // A position has at most 64 set bits, which fit in a `usize`.
        let expected_ommers = frontier.position.count_ones() as usize;
        if frontier.ommers.len() + sibling != expected_ommers {
            return Err(invalid(
                component,
                "the frontier's ommers do not match its position",
            ));
        }
        nodes.extend(&frontier.ommers);
    }
    if !nodes.into_iter().all(node_is_valid) {
        return Err(invalid(
            component,
            "a node of the tree is not a canonical field element",
        ));
    }

    options
        .deserialize(value)
        .map_err(|_| invalid(component, "the value is not a note commitment tree"))
}
