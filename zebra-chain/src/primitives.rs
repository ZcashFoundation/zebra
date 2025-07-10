//! External primitives used in Zcash structures.
//!
//! This contains re-exports of libraries used in the public API, as well as stub
//! definitions of primitive types which must be represented in this library but
//! whose functionality is implemented elsewhere.

mod proofs;

mod address;

pub use address::Address;

pub mod byte_array;

pub use ed25519_zebra as ed25519;
pub use reddsa;
pub use redjubjub;
pub use x25519_dalek as x25519;

pub use proofs::{Bctv14Proof, Groth16Proof, Halo2Proof, ZkSnarkProof};

pub mod zcash_history;
pub mod zcash_note_encryption;
pub(crate) mod zcash_primitives;
