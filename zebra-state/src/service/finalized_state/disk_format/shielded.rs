//! Shielded transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::sync::Arc;

use bincode::Options;

use zebra_chain::{
    block::Height,
    orchard::{self, AssetBase},
    orchard_zsa::{self, BurnItem},
    sapling, sprout,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::Transaction,
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

use super::block::HEIGHT_DISK_BYTES;

/// The circulating supply and whether that supply has been finalized.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetState {
    /// Indicates whether the asset is finalized such that no more of it can be issued.
    pub is_finalized: bool,
    /// The circulating supply that has been issued for an asset.
    pub total_supply: i128,
}

impl AssetState {
    /// Adds partial asset states
    pub fn with_change(&self, change: Self) -> Self {
        Self {
            is_finalized: self.is_finalized || change.is_finalized,
            total_supply: self
                .total_supply
                .checked_add(change.total_supply)
                .expect("asset supply sum should not exceed u64 size"),
        }
    }

    /// Adds partial asset states
    pub fn with_revert(&self, change: Self) -> Self {
        Self {
            is_finalized: self.is_finalized && !change.is_finalized,
            total_supply: self
                .total_supply
                .checked_sub(change.total_supply)
                .expect("change should be less than total supply"),
        }
    }

    pub fn from_note(is_finalized: bool, note: orchard_zsa::Note) -> (AssetBase, Self) {
        (
            note.asset(),
            Self {
                is_finalized,
                total_supply: note.value().inner().into(),
            },
        )
    }

    pub fn from_notes(
        is_finalized: bool,
        notes: &[orchard_zsa::Note],
    ) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        notes
            .iter()
            .map(move |note| Self::from_note(is_finalized, *note))
    }

    pub fn from_burn(burn: BurnItem) -> (AssetBase, Self) {
        (
            burn.asset(),
            Self {
                is_finalized: false,
                total_supply: -burn.amount().as_i128(),
            },
        )
    }

    pub fn from_burns(burns: Vec<BurnItem>) -> impl Iterator<Item = (AssetBase, Self)> {
        burns.into_iter().map(Self::from_burn)
    }

    pub fn from_transaction(
        transaction: &Arc<Transaction>,
    ) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        transaction
            .issue_actions()
            .flat_map(|action| AssetState::from_notes(action.is_finalized(), action.notes()))
            .chain(AssetState::from_burns(transaction.burns()))
    }
}

impl IntoDisk for AssetState {
    type Bytes = [u8; 9];

    fn as_bytes(&self) -> Self::Bytes {
        [
            vec![self.is_finalized as u8],
            self.total_supply.to_be_bytes().to_vec(),
        ]
        .concat()
        .try_into()
        .unwrap()
    }
}

impl FromDisk for AssetState {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let (&is_finalized_byte, bytes) = bytes.as_ref().split_first().unwrap();
        let (&total_supply_bytes, _bytes) = bytes.split_first_chunk().unwrap();

        Self {
            is_finalized: is_finalized_byte != 0,
            total_supply: u64::from_be_bytes(total_supply_bytes).into(),
        }
    }
}

impl IntoDisk for orchard::AssetBase {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.to_bytes()
    }
}

impl FromDisk for orchard::AssetBase {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let (asset_base_bytes, _) = bytes.as_ref().split_first_chunk().unwrap();
        Self::from_bytes(asset_base_bytes).unwrap()
    }
}

impl IntoDisk for sprout::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        *self.0
    }
}

impl IntoDisk for sapling::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        *self.0
    }
}

impl IntoDisk for orchard::Nullifier {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        let nullifier: orchard::Nullifier = *self;
        nullifier.into()
    }
}

impl IntoDisk for sprout::tree::Root {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.into()
    }
}

impl FromDisk for sprout::tree::Root {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array: [u8; 32] = bytes.as_ref().try_into().unwrap();
        array.into()
    }
}

impl IntoDisk for sapling::tree::Root {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.into()
    }
}

impl FromDisk for sapling::tree::Root {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array: [u8; 32] = bytes.as_ref().try_into().unwrap();
        array.try_into().expect("finalized data must be valid")
    }
}

impl IntoDisk for orchard::tree::Root {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        self.into()
    }
}

impl FromDisk for orchard::tree::Root {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array: [u8; 32] = bytes.as_ref().try_into().unwrap();
        array.try_into().expect("finalized data must be valid")
    }
}

impl IntoDisk for NoteCommitmentSubtreeIndex {
    type Bytes = [u8; 2];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
}

impl FromDisk for NoteCommitmentSubtreeIndex {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array: [u8; 2] = bytes.as_ref().try_into().unwrap();
        Self(u16::from_be_bytes(array))
    }
}

// The following implementations for the note commitment trees use `serde` and
// `bincode`. `serde` serializations depend on the inner structure of the type.
// They should not be used in new code. (This is an issue for any derived serialization format.)
//
// We explicitly use `bincode::DefaultOptions`  to disallow trailing bytes; see
// https://docs.rs/bincode/1.3.3/bincode/config/index.html#options-struct-vs-bincode-functions

impl IntoDisk for sprout::tree::NoteCommitmentTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for sprout::tree::NoteCommitmentTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}
impl IntoDisk for sapling::tree::NoteCommitmentTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for sapling::tree::NoteCommitmentTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

impl IntoDisk for orchard::tree::NoteCommitmentTree {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        bincode::DefaultOptions::new()
            .serialize(self)
            .expect("serialization to vec doesn't fail")
    }
}

impl FromDisk for orchard::tree::NoteCommitmentTree {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bincode::DefaultOptions::new()
            .deserialize(bytes.as_ref())
            .expect("deserialization format should match the serialization format used by IntoDisk")
    }
}

impl IntoDisk for sapling::tree::Node {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.as_ref().to_vec()
    }
}

impl IntoDisk for orchard::tree::Node {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.to_repr().to_vec()
    }
}

impl<Root: IntoDisk<Bytes = Vec<u8>>> IntoDisk for NoteCommitmentSubtreeData<Root> {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        [self.end_height.as_bytes().to_vec(), self.root.as_bytes()].concat()
    }
}

impl FromDisk for sapling::tree::Node {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self::try_from(bytes.as_ref()).expect("trusted data should deserialize successfully")
    }
}

impl FromDisk for orchard::tree::Node {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self::try_from(bytes.as_ref()).expect("trusted data should deserialize successfully")
    }
}

impl<Node: FromDisk> FromDisk for NoteCommitmentSubtreeData<Node> {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (height_bytes, node_bytes) = disk_bytes.as_ref().split_at(HEIGHT_DISK_BYTES);
        Self::new(
            Height::from_bytes(height_bytes),
            Node::from_bytes(node_bytes),
        )
    }
}
