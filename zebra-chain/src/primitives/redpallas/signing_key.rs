use std::convert::TryFrom;

use halo2::arithmetic::FieldExt;
use halo2::pasta::pallas;

use super::{Error, SigType, VerificationKey};

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

impl<'a, T: SigType> From<&'a SigningKey<T>> for VerificationKey<T> {
    fn from(sk: &'a SigningKey<T>) -> VerificationKey<T> {
        sk.pk
    }
}

impl<T: SigType> TryFrom<[u8; 32]> for SigningKey<T> {
    type Error = Error;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let maybe_sk = pallas::Scalar::from_bytes(&bytes);

        if maybe_sk.is_some().into() {
            let sk = maybe_sk.unwrap();
            let pk = VerificationKey::from(&sk);
            Ok(SigningKey { sk, pk })
        } else {
            Err(Error::MalformedSigningKey)
        }
    }
}
