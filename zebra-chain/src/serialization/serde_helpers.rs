use crate::primitives::ed25519;
use group::{ff::PrimeField, GroupEncoding};
use halo2::pasta::pallas;
use hex::{FromHex, ToHex};
use redjubjub::SigType;

#[derive(Deserialize, Serialize)]
#[serde(remote = "jubjub::AffinePoint")]
pub struct AffinePoint {
    #[serde(getter = "jubjub::AffinePoint::to_bytes")]
    bytes: [u8; 32],
}

impl From<AffinePoint> for jubjub::AffinePoint {
    fn from(local: AffinePoint) -> Self {
        jubjub::AffinePoint::from_bytes(local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "jubjub::Fq")]
pub struct Fq {
    #[serde(getter = "jubjub::Fq::to_bytes")]
    bytes: [u8; 32],
}

impl From<Fq> for jubjub::Fq {
    fn from(local: Fq) -> Self {
        jubjub::Fq::from_bytes(&local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "pallas::Affine")]
pub struct Affine {
    #[serde(getter = "pallas::Affine::to_bytes")]
    bytes: [u8; 32],
}

impl From<Affine> for pallas::Affine {
    fn from(local: Affine) -> Self {
        pallas::Affine::from_bytes(&local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "pallas::Scalar")]
pub struct Scalar {
    #[serde(getter = "pallas::Scalar::to_repr")]
    bytes: [u8; 32],
}

impl From<Scalar> for pallas::Scalar {
    fn from(local: Scalar) -> Self {
        pallas::Scalar::from_repr(local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "pallas::Base")]
pub struct Base {
    #[serde(getter = "pallas::Base::to_repr")]
    bytes: [u8; 32],
}

impl From<Base> for pallas::Base {
    fn from(local: Base) -> Self {
        pallas::Base::from_repr(local.bytes).unwrap()
    }
}

/// A wrapper around [`redjubjub::Signature`] to provide hexadecimal 
/// serialization and deserialization
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct HexSignature<T: SigType>(pub redjubjub::Signature<T>);

impl<T: SigType> FromHex for HexSignature<T> {
    type Error = hex::FromHexError;

    fn from_hex<H: AsRef<[u8]>>(hex: H) -> Result<Self, Self::Error> {
        let bytes: [u8; 64] = hex::decode(hex.as_ref())?
            .try_into()
            .map_err(|_| hex::FromHexError::InvalidStringLength)?;

        Ok(HexSignature(redjubjub::Signature::from(bytes)))
    }
}

impl<T: SigType> ToHex for HexSignature<T> {
    fn encode_hex<H: FromIterator<char>>(&self) -> H {
        let bytes: [u8; 64] = self.0.into();
        bytes.encode_hex()
    }

    fn encode_hex_upper<H: FromIterator<char>>(&self) -> H {
        let bytes: [u8; 64] = self.0.into();
        bytes.encode_hex_upper()
    }
}

impl<T: SigType> From<redjubjub::Signature<T>> for HexSignature<T> {
    fn from(sig: redjubjub::Signature<T>) -> Self {
        HexSignature(sig)
    }
}

impl<T: SigType> From<HexSignature<T>> for redjubjub::Signature<T> {
    fn from(sig: HexSignature<T>) -> Self {
        sig.0
    }
}

/// A wrapper for a fixed-size byte array to provide hexadecimal 
/// serialization and deserialization.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct HexBytes<const N: usize>(pub [u8; N]);

impl<const N: usize> FromHex for HexBytes<N> {
    type Error = hex::FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let decoded_bytes = hex::decode(hex.as_ref())?;
        let bytes: [u8; N] = decoded_bytes
            .try_into()
            .map_err(|_| hex::FromHexError::InvalidStringLength)?;
        Ok(HexBytes(bytes))
    }
}

impl<const N: usize> ToHex for HexBytes<N> {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.0.encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.0.encode_hex_upper()
    }
}

impl From<ed25519::VerificationKeyBytes> for HexBytes<32> {
    fn from(key: ed25519::VerificationKeyBytes) -> Self {
        HexBytes(key.into())
    }
}

impl From<HexBytes<32>> for ed25519::VerificationKeyBytes {
    fn from(hex_bytes: HexBytes<32>) -> Self {
        hex_bytes.0.into()
    }
}

impl From<ed25519::Signature> for HexBytes<64> {
    fn from(sig: ed25519::Signature) -> Self {
        HexBytes(sig.into())
    }
}

impl From<HexBytes<64>> for ed25519::Signature {
    fn from(hex_bytes: HexBytes<64>) -> Self {
        hex_bytes.0.into()
    }
}
