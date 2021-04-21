//! ZK proofs used in Zcash.

use std::fmt::Debug;

use serde::{de::DeserializeOwned, Serialize};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};

mod bctv14;
mod groth16;
mod halo2;

pub use self::bctv14::Bctv14Proof;
pub use self::groth16::Groth16Proof;
pub use self::halo2::Halo2Proof;

/// A marker trait used to abstract over BCTV14, Groth16, or Halo2 proofs.
pub trait ZkSnarkProof:
    Clone
    + Debug
    + PartialEq
    + Eq
    + Serialize
    + DeserializeOwned
    + ZcashSerialize
    + ZcashDeserialize
    + private::Sealed
{
}
impl ZkSnarkProof for Bctv14Proof {}
impl ZkSnarkProof for Groth16Proof {}
impl ZkSnarkProof for Halo2Proof {}

mod private {
    use super::*;

    pub trait Sealed {}
    impl Sealed for Bctv14Proof {}
    impl Sealed for Groth16Proof {}
    impl Sealed for Halo2Proof {}
}
