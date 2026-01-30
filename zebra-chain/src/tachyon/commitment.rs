//! Tachyon note and value commitments.
//!
//! These are similar to Orchard commitments but used in the Tachyon shielded pool.
//!
//! ## Note Commitment Representations
//!
//! There are two note commitment types:
//! - [`NoteCommitment`] (this module) - stores the full `pallas::Affine` curve point
//!   for blockchain serialization
//! - [`tachyon::NoteCommitment`] - stores just the x-coordinate (`Fp`) for the
//!   polynomial accumulator
//!
//! Use [`NoteCommitment::to_tachyon`] to convert from the full point to the extracted
//! x-coordinate for accumulator operations.

use std::{fmt, io, iter::Sum, ops};

use group::{ff::PrimeField, prime::PrimeCurveAffine, GroupEncoding};
use halo2::{
    arithmetic::{Coordinates, CurveAffine},
    pasta::pallas,
};

use crate::{
    amount::Amount,
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

/// Note commitment for Tachyon notes.
///
/// Similar to Orchard note commitments, this is a point on the Pallas curve
/// representing a commitment to a note's contents.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteCommitment(#[serde(with = "serde_helpers::Affine")] pub pallas::Affine);

impl NoteCommitment {
    /// Extract the x-coordinate of the note commitment.
    ///
    /// This is what gets serialized in actions and committed to the tree.
    pub fn extract_x(&self) -> pallas::Base {
        // For non-identity points, return the x-coordinate.
        // For identity, return zero (same as Orchard behavior).
        let coords: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();
        coords
            .map(|c| *c.x())
            .unwrap_or_else(pallas::Base::zero)
    }

    /// Extract the x-coordinate as bytes.
    pub fn extract_x_bytes(&self) -> [u8; 32] {
        self.extract_x().to_repr()
    }

    /// Convert to tachyon crate NoteCommitment (extracted x-coordinate).
    ///
    /// The tachyon crate represents note commitments as field elements (the x-coordinate),
    /// which is what gets accumulated in the polynomial accumulator.
    pub fn to_tachyon(&self) -> tachyon::NoteCommitment {
        tachyon::NoteCommitment::from_field(self.extract_x())
    }
}

impl fmt::Debug for NoteCommitment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let coords: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match coords {
            Some(c) => f
                .debug_struct("tachyon::NoteCommitment")
                .field("x", &hex::encode(c.x().to_repr()))
                .field("y", &hex::encode(c.y().to_repr()))
                .finish(),
            None => f
                .debug_struct("tachyon::NoteCommitment")
                .field("identity", &true)
                .finish(),
        }
    }
}

impl From<pallas::Point> for NoteCommitment {
    fn from(point: pallas::Point) -> Self {
        Self(pallas::Affine::from(point))
    }
}

impl From<NoteCommitment> for [u8; 32] {
    fn from(cm: NoteCommitment) -> Self {
        cm.0.to_bytes()
    }
}

impl TryFrom<[u8; 32]> for NoteCommitment {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Affine::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid pallas::Affine value for NoteCommitment")
        }
    }
}

/// Value commitment for Tachyon balance proofs.
///
/// A homomorphic Pedersen commitment to the net value of an action,
/// used to prove balance without revealing amounts.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueCommitment(#[serde(with = "serde_helpers::Affine")] pub pallas::Affine);

/// GroupHash into Pallas for Tachyon value commitments.
///
/// Produces a random point in the Pallas curve using the hash_to_curve method.
#[allow(non_snake_case)]
fn pallas_group_hash(D: &[u8], M: &[u8]) -> pallas::Point {
    use halo2::arithmetic::CurveExt;
    let domain_separator = std::str::from_utf8(D).expect("domain must be valid UTF-8");
    pallas::Point::hash_to_curve(domain_separator)(M)
}

impl ValueCommitment {
    /// Create a new ValueCommitment from a scalar (randomness) and value.
    ///
    /// `ValueCommit(v, rcv) = [v] V + [rcv] R`
    ///
    /// where V and R are generator points.
    #[allow(non_snake_case)]
    pub fn new(rcv: pallas::Scalar, value: Amount) -> Self {
        use lazy_static::lazy_static;

        lazy_static! {
            // TODO: Use Tachyon-specific domain separators once specified
            static ref V: pallas::Point = pallas_group_hash(b"z.cash:Tachyon-cv", b"v");
            static ref R: pallas::Point = pallas_group_hash(b"z.cash:Tachyon-cv", b"r");
        }

        let v = pallas::Scalar::from(value);
        Self::from(*V * v + *R * rcv)
    }
}

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let coords: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match coords {
            Some(c) => f
                .debug_struct("tachyon::ValueCommitment")
                .field("x", &hex::encode(c.x().to_repr()))
                .field("y", &hex::encode(c.y().to_repr()))
                .finish(),
            None => f
                .debug_struct("tachyon::ValueCommitment")
                .field("identity", &true)
                .finish(),
        }
    }
}

impl From<pallas::Point> for ValueCommitment {
    fn from(point: pallas::Point) -> Self {
        Self(pallas::Affine::from(point))
    }
}

impl From<ValueCommitment> for [u8; 32] {
    fn from(cv: ValueCommitment) -> Self {
        cv.0.to_bytes()
    }
}

impl TryFrom<[u8; 32]> for ValueCommitment {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Affine::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid pallas::Affine value for ValueCommitment")
        }
    }
}

impl ops::Add for ValueCommitment {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self((self.0 + rhs.0).into())
    }
}

impl ops::Sub for ValueCommitment {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self((self.0 - rhs.0).into())
    }
}

impl Sum for ValueCommitment {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(
            ValueCommitment(pallas::Affine::identity()),
            ops::Add::add,
        )
    }
}

impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Self::try_from(reader.read_32_bytes()?).map_err(SerializationError::Parse)
    }
}

#[cfg(test)]
mod tests {
    use group::Group;

    use super::*;

    #[test]
    fn value_commitment_add() {
        let _init_guard = zebra_test::init();

        let identity = ValueCommitment(pallas::Affine::identity());
        let g = ValueCommitment(pallas::Affine::generator());

        assert_eq!(identity + g, g);
    }

    #[test]
    fn value_commitment_sum() {
        let _init_guard = zebra_test::init();

        let g = ValueCommitment(pallas::Affine::generator());
        let sum: ValueCommitment = vec![g, g].into_iter().sum();

        let doubled = ValueCommitment(pallas::Affine::generator().to_curve().double().into());
        assert_eq!(sum, doubled);
    }
}
