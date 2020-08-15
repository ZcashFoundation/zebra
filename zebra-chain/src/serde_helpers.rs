use super::equihash_solution::EQUIHASH_SOLUTION_SIZE;
use serde_big_array::big_array;

big_array! {
    BigArray;
    +EQUIHASH_SOLUTION_SIZE,
    80, // `sapling::OutCiphertext`
    580, // `sapling::EncryptedCiphertext`
    601, // `sprout::EncryptedCiphertext`
    296, // `bctv14::Bctv14Proof`
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "jubjub::AffinePoint")]
pub(crate) struct AffinePoint {
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
pub(crate) struct Fq {
    #[serde(getter = "jubjub::Fq::to_bytes")]
    bytes: [u8; 32],
}

impl From<Fq> for jubjub::Fq {
    fn from(local: Fq) -> Self {
        jubjub::Fq::from_bytes(&local.bytes).unwrap()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "futures::future::Either")]
pub(crate) enum Either<A, B> {
    Left(A),
    Right(B),
}
