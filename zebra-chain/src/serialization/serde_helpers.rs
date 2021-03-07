use halo2::pasta::pallas;
use serde_big_array::big_array;

big_array! {
    BigArray;
    + 1344, // `EquihashSolution`
    80,   // `sapling::OutCiphertext`
    580,  // `sapling::EncryptedCiphertext`
    601,  // `sprout::EncryptedCiphertext`
    296,  // `Bctv14Proof`
    196,  // `Groth16Proof`
}

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
        pallas::Affine::from_bytes(local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "pallas::Scalar")]
pub struct Scalar {
    #[serde(getter = "pallas::Scalar::to_bytes")]
    bytes: [u8; 32],
}

impl From<Scalar> for pallas::Scalar {
    fn from(local: Scalar) -> Self {
        pallas::Scalar::from_bytes(&local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "futures::future::Either")]
pub enum Either<A, B> {
    Left(A),
    Right(B),
}
