//! Shielded transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use bincode::Options;

use zebra_chain::{orchard, sapling, sprout};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

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

// The following implementations for the note commitment trees use `serde` and
// `bincode` because currently the inner Merkle tree frontier (from
// `incrementalmerkletree`) only supports `serde` for serialization. `bincode`
// was chosen because it is small and fast. We explicitly use `DefaultOptions`
// in particular to disallow trailing bytes; see
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
