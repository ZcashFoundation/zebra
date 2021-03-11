use std::{
    convert::{TryFrom, TryInto},
    marker::PhantomData,
};

use halo2::pasta::pallas;

use super::{SigType, VerificationKey};

/// A RedPallas signing key.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "SerdeHelper"))]
#[cfg_attr(feature = "serde", serde(into = "SerdeHelper"))]
#[cfg_attr(feature = "serde", serde(bound = "T: SigType"))]
pub struct SigningKey<T: SigType> {
    sk: pallas::Scalar,
    pk: VerificationKey<T>,
}
