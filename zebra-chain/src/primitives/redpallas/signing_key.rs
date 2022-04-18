use std::convert::{TryFrom, TryInto};
use std::marker::PhantomData;

use group::{ff::PrimeField, GroupEncoding};
use halo2::{arithmetic::FieldExt, pasta::pallas};
use rand_core::{CryptoRng, RngCore};

use super::{Error, SigType, Signature, SpendAuth, VerificationKey};

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

impl<T: SigType> From<SigningKey<T>> for [u8; 32] {
    fn from(sk: SigningKey<T>) -> [u8; 32] {
        sk.sk.to_repr()
    }
}

impl<T: SigType> TryFrom<[u8; 32]> for SigningKey<T> {
    type Error = Error;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let maybe_sk = pallas::Scalar::from_repr(bytes);

        if maybe_sk.is_some().into() {
            let sk = maybe_sk.unwrap();
            let pk = VerificationKey::from_scalar(&sk);
            Ok(SigningKey { sk, pk })
        } else {
            Err(Error::MalformedSigningKey)
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct SerdeHelper([u8; 32]);

impl<T: SigType> TryFrom<SerdeHelper> for SigningKey<T> {
    type Error = Error;

    fn try_from(helper: SerdeHelper) -> Result<Self, Self::Error> {
        helper.0.try_into()
    }
}

impl<T: SigType> From<SigningKey<T>> for SerdeHelper {
    fn from(sk: SigningKey<T>) -> Self {
        Self(sk.into())
    }
}

impl SigningKey<SpendAuth> {
    /// Randomize this public key with the given `randomizer`.
    pub fn randomize(&self, randomizer: &pallas::Scalar) -> SigningKey<SpendAuth> {
        let sk = self.sk + randomizer;
        let pk = VerificationKey::from_scalar(&sk);
        SigningKey { sk, pk }
    }
}

impl<T: SigType> SigningKey<T> {
    /// Generate a new signing key.
    pub fn new<R: RngCore + CryptoRng>(mut rng: R) -> SigningKey<T> {
        let sk = {
            let mut bytes = [0; 64];
            rng.fill_bytes(&mut bytes);
            pallas::Scalar::from_bytes_wide(&bytes)
        };
        let pk = VerificationKey::from_scalar(&sk);
        SigningKey { sk, pk }
    }

    /// Create a signature of type `T` on `msg` using this `SigningKey`.
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#concretereddsa
    // Similar to signature::Signer but without boxed errors.
    pub fn sign<R: RngCore + CryptoRng>(&self, mut rng: R, msg: &[u8]) -> Signature<T> {
        use super::HStar;

        // RedDSA.GenRandom:() → R RedDSA.Random
        // Choose a byte sequence uniformly at random of length
        // (\ell_H + 128)/8 bytes.  For RedPallas this is (512 + 128)/8 = 80.
        let random_bytes = {
            let mut bytes = [0; 80];
            rng.fill_bytes(&mut bytes);
            bytes
        };

        let nonce = HStar::default()
            .update(&random_bytes[..])
            .update(&self.pk.bytes.bytes[..]) // XXX ugly
            .update(msg)
            .finalize();

        let r_bytes = pallas::Affine::from(T::basepoint() * nonce).to_bytes();

        let c = HStar::default()
            .update(&r_bytes[..])
            .update(&self.pk.bytes.bytes[..]) // XXX ugly
            .update(msg)
            .finalize();

        let s_bytes = (nonce + (c * self.sk)).to_repr();

        Signature {
            r_bytes,
            s_bytes,
            _marker: PhantomData,
        }
    }
}
