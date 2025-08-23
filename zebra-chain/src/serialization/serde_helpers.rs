use group::{ff::PrimeField, GroupEncoding};
use halo2::pasta::pallas;

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

#[derive(Deserialize, Serialize)]
#[serde(remote = "sapling_crypto::value::ValueCommitment")]
pub struct ValueCommitment {
    #[serde(getter = "sapling_crypto::value::ValueCommitment::to_bytes")]
    bytes: [u8; 32],
}

impl From<ValueCommitment> for sapling_crypto::value::ValueCommitment {
    fn from(local: ValueCommitment) -> Self {
        sapling_crypto::value::ValueCommitment::from_bytes_not_small_order(&local.bytes).unwrap()
    }
}
