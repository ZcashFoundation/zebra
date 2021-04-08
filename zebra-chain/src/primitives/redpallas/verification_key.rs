use std::{
    convert::TryFrom,
    // hash::{Hash, Hasher},
    marker::PhantomData,
};

use group::{cofactor::CofactorGroup, GroupEncoding};
use halo2::pasta::pallas;

use super::{Error, SigType};

/// A refinement type for `[u8; 32]` indicating that the bytes represent
/// an encoding of a RedPallas verification key.
///
/// This is useful for representing a compressed verification key; the
/// [`VerificationKey`] type in this library holds other decompressed state
/// used in signature verification.
#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationKeyBytes<T: SigType> {
    pub(crate) bytes: [u8; 32],
    pub(crate) _marker: PhantomData<T>,
}

impl<T: SigType> From<[u8; 32]> for VerificationKeyBytes<T> {
    fn from(bytes: [u8; 32]) -> VerificationKeyBytes<T> {
        VerificationKeyBytes {
            bytes,
            _marker: PhantomData,
        }
    }
}

impl<T: SigType> From<VerificationKeyBytes<T>> for [u8; 32] {
    fn from(refined: VerificationKeyBytes<T>) -> [u8; 32] {
        refined.bytes
    }
}

// impl<T: SigType> Hash for VerificationKeyBytes<T> {
//     fn hash<H: Hasher>(&self, state: &mut H) {
//         self.bytes.hash(state);
//         self._marker.hash(state);
//     }
// }

/// A valid RedPallas verification key.
///
/// This type holds decompressed state used in signature verification; if the
/// verification key may not be used immediately, it is probably better to use
/// [`VerificationKeyBytes`], which is a refinement type for `[u8; 32]`.
///
/// ## Consensus properties
///
/// The `TryFrom<VerificationKeyBytes>` conversion performs the following Zcash
/// consensus rule checks:
///
/// 1. The check that the bytes are a canonical encoding of a verification key;
/// 2. The check that the verification key is not a point of small order.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "VerificationKeyBytes<T>"))]
#[cfg_attr(feature = "serde", serde(into = "VerificationKeyBytes<T>"))]
#[cfg_attr(feature = "serde", serde(bound = "T: SigType"))]
pub struct VerificationKey<T: SigType> {
    pub(crate) point: pallas::Point,
    pub(crate) bytes: VerificationKeyBytes<T>,
}

impl<T: SigType> From<VerificationKey<T>> for VerificationKeyBytes<T> {
    fn from(pk: VerificationKey<T>) -> VerificationKeyBytes<T> {
        pk.bytes
    }
}

impl<T: SigType> From<VerificationKey<T>> for [u8; 32] {
    fn from(pk: VerificationKey<T>) -> [u8; 32] {
        pk.bytes.bytes
    }
}

impl<T: SigType> From<&pallas::Scalar> for VerificationKey<T> {
    fn from(s: &pallas::Scalar) -> VerificationKey<T> {
        let point = T::basepoint() * s;
        let bytes = VerificationKeyBytes {
            bytes: pallas::Affine::from(&point).to_bytes(),
            _marker: PhantomData,
        };

        Self { point, bytes }
    }
}

impl<T: SigType> TryFrom<VerificationKeyBytes<T>> for VerificationKey<T> {
    type Error = Error;

    fn try_from(bytes: VerificationKeyBytes<T>) -> Result<Self, Self::Error> {
        // This checks that the encoding is canonical...
        let maybe_point = pallas::Affine::from_bytes(&bytes.bytes);

        if maybe_point.is_some().into() {
            let point: pallas::Point = maybe_point.unwrap().into();

            // This checks that the verification key is not of small order.
            if !<bool>::from(point.is_small_order()) {
                Ok(VerificationKey { point, bytes })
            } else {
                Err(Error::MalformedVerificationKey)
            }
        } else {
            Err(Error::MalformedVerificationKey)
        }
    }
}

impl<T: SigType> TryFrom<[u8; 32]> for VerificationKey<T> {
    type Error = Error;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        use std::convert::TryInto;
        VerificationKeyBytes::from(bytes).try_into()
    }
}
