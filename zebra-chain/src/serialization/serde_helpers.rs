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
#[serde(remote = "orchard::note::ExtractedNoteCommitment")]
pub struct ExtractedNoteCommitment {
    #[serde(getter = "temp_convert_fn")]
    bytes: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(remote = "sapling_crypto::note::ExtractedNoteCommitment")]
pub struct SaplingExtractedNoteCommitment {
    #[serde(getter = "temp_sapling_convert_fn")]
    bytes: [u8; 32],
}

impl From<SaplingExtractedNoteCommitment> for sapling_crypto::note::ExtractedNoteCommitment {
    fn from(local: SaplingExtractedNoteCommitment) -> Self {
        sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&local.bytes).unwrap()
    }
}

// TODO: Replace this with something reasonable like a method.
fn temp_sapling_convert_fn(cm_u: &sapling_crypto::note::ExtractedNoteCommitment) -> [u8; 32] {
    cm_u.to_bytes()
}

// TODO: Replace this with something reasonable like a method.
fn temp_convert_fn(cm_x: &orchard::note::ExtractedNoteCommitment) -> [u8; 32] {
    cm_x.to_bytes()
}

impl From<ExtractedNoteCommitment> for orchard::note::ExtractedNoteCommitment {
    fn from(local: ExtractedNoteCommitment) -> Self {
        orchard::note::ExtractedNoteCommitment::from_bytes(&local.bytes).unwrap()
    }
}
