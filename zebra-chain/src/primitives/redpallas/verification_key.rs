use std::{
    convert::TryFrom,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

use halo2::pasta::pallas;

use super::{Error, SigType, Signature, SpendAuth};

/// A refinement type for `[u8; 32]` indicating that the bytes represent
/// an encoding of a RedPallas verification key.
///
/// This is useful for representing a compressed verification key; the
/// [`VerificationKey`] type in this library holds other decompressed state
/// used in signature verification.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

impl<T: SigType> Hash for VerificationKeyBytes<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
        self._marker.hash(state);
    }
}

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

impl<T: SigType> TryFrom<VerificationKeyBytes<T>> for VerificationKey<T> {
    type Error = Error;

    fn try_from(bytes: VerificationKeyBytes<T>) -> Result<Self, Self::Error> {
        // XXX-pasta-curves: this should not use CtOption
        // XXX-pasta-curves: this takes ownership of bytes, while Fr doesn't.
        // This checks that the encoding is canonical...
        let maybe_point = pallas::Point::from_bytes(bytes.bytes);
        if maybe_point.is_some().into() {
            let point = maybe_point.unwrap();
            // This checks that the verification key is not of small order.
            if <bool>::from(point.is_small_order()) == false {
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

impl VerificationKey<SpendAuth> {
    /// Randomize this verification key with the given `randomizer`.
    ///
    /// Randomization is only supported for `SpendAuth` keys.
    pub fn randomize(&self, randomizer: &Randomizer) -> VerificationKey<SpendAuth> {
        use crate::private::Sealed;
        let point = self.point + &(&SpendAuth::basepoint() * randomizer);
        let bytes = VerificationKeyBytes {
            bytes: point.to_bytes().as_ref().try_into().unwrap(),
            _marker: PhantomData,
        };
        VerificationKey { bytes, point }
    }
}

impl<T: SigType> VerificationKey<T> {
    pub(crate) fn from(s: &pallas::Scalar) -> VerificationKey<T> {
        let point = &T::basepoint() * s;
        let bytes = VerificationKeyBytes {
            bytes: point.to_bytes().as_ref().try_into().unwrap(),
            _marker: PhantomData,
        };
        VerificationKey { bytes, point }
    }

    /// Verify a purported `signature` over `msg` made by this verification key.
    // This is similar to impl signature::Verifier but without boxed errors
    pub fn verify(&self, msg: &[u8], signature: &Signature<T>) -> Result<(), Error> {
        use crate::HStar;
        let c = HStar::default()
            .update(&signature.r_bytes[..])
            .update(&self.bytes.bytes[..]) // XXX ugly
            .update(msg)
            .finalize();
        self.verify_prehashed(signature, c)
    }

    /// Verify a purported `signature` with a prehashed challenge.
    #[allow(non_snake_case)]
    pub(crate) fn verify_prehashed(
        &self,
        signature: &Signature<T>,
        c: pallas::Scalar,
    ) -> Result<(), Error> {
        let r = {
            // XXX-pasta-curves: should not use CtOption here
            // XXX-pasta-curves: inconsistent ownership in from_bytes
            let maybe_point = pallas::Point::from_bytes(signature.r_bytes);
            if maybe_point.is_some().into() {
                maybe_point.unwrap()
            } else {
                return Err(Error::InvalidSignature);
            }
        };

        let s = {
            // XXX-pasta-curves: should not use CtOption here
            let maybe_scalar = pallas::Scalar::from_bytes(&signature.s_bytes);
            if maybe_scalar.is_some().into() {
                maybe_scalar.unwrap()
            } else {
                return Err(Error::InvalidSignature);
            }
        };

        // XXX rewrite as normal double scalar mul
        // Verify check is h * ( - s * B + R  + c * A) == 0
        //                 h * ( s * B - c * A - R) == 0
        let sB = T::basepoint() * s;
        let cA = self.point * c;
        let check = sB - cA - r;

        if check.is_small_order().into() {
            Ok(())
        } else {
            Err(Error::InvalidSignature)
        }
    }
}
