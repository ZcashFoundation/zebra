//! External primitives used in Zcash structures.
//!
//! This contains re-exports of libraries used in the public API, as well as stub
//! definitions of primitive types which must be represented in this library but
//! whose functionality is implemented elsewhere.

mod proofs;
// TODO: re-export redpallas if needed, or reddsa if that gets merged https://github.com/ZcashFoundation/zebra/issues/2044
pub mod redpallas;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

pub use ed25519_zebra as ed25519;
pub use redjubjub;
pub use x25519_dalek as x25519;

pub use proofs::{Bctv14Proof, Groth16Proof, Halo2Proof, ZkSnarkProof};
