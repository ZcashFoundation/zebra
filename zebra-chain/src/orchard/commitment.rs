//! Note and value commitments.

use std::{fmt, io};

use group::{ff::PrimeField, prime::PrimeCurveAffine, GroupEncoding};
use halo2::{
    arithmetic::{Coordinates, CurveAffine},
    pasta::pallas,
};

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

/// The randomness used in the Simsemilla hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommitmentRandomness(pallas::Scalar);

/// A homomorphic Pedersen commitment to the net value of a _note_, used in
/// Action descriptions.
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretehomomorphiccommit>
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
pub struct ValueCommitment(#[serde(with = "serde_helpers::Affine")] pub pallas::Affine);

impl<'a> std::ops::Add<&'a ValueCommitment> for ValueCommitment {
    type Output = Self;

    fn add(self, rhs: &'a ValueCommitment) -> Self::Output {
        self + *rhs
    }
}

impl std::ops::Add<ValueCommitment> for ValueCommitment {
    type Output = Self;

    fn add(self, rhs: ValueCommitment) -> Self::Output {
        ValueCommitment((self.0 + rhs.0).into())
    }
}

impl std::ops::AddAssign<ValueCommitment> for ValueCommitment {
    fn add_assign(&mut self, rhs: ValueCommitment) {
        *self = *self + rhs
    }
}

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct("ValueCommitment");

        let option: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match option {
            Some(coordinates) => d
                .field("x", &hex::encode(coordinates.x().to_repr()))
                .field("y", &hex::encode(coordinates.y().to_repr()))
                .finish(),
            None => d
                .field("x", &hex::encode(pallas::Base::zero().to_repr()))
                .field("y", &hex::encode(pallas::Base::zero().to_repr()))
                .finish(),
        }
    }
}

impl From<pallas::Point> for ValueCommitment {
    fn from(projective_point: pallas::Point) -> Self {
        Self(pallas::Affine::from(projective_point))
    }
}

/// LEBS2OSP256(repr_P(cv))
///
/// <https://zips.z.cash/protocol/nu5.pdf#pallasandvesta>
impl From<ValueCommitment> for [u8; 32] {
    fn from(cm: ValueCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

impl<'a> std::ops::Sub<&'a ValueCommitment> for ValueCommitment {
    type Output = Self;

    fn sub(self, rhs: &'a ValueCommitment) -> Self::Output {
        self - *rhs
    }
}

impl std::ops::Sub<ValueCommitment> for ValueCommitment {
    type Output = Self;

    fn sub(self, rhs: ValueCommitment) -> Self::Output {
        ValueCommitment((self.0 - rhs.0).into())
    }
}

impl std::ops::SubAssign<ValueCommitment> for ValueCommitment {
    fn sub_assign(&mut self, rhs: ValueCommitment) {
        *self = *self - rhs;
    }
}

impl std::iter::Sum for ValueCommitment {
    fn sum<I>(iter: I) -> Self
    where
        I: Iterator<Item = Self>,
    {
        iter.fold(
            ValueCommitment(pallas::Affine::identity()),
            std::ops::Add::add,
        )
    }
}

/// LEBS2OSP256(repr_P(cv))
///
/// <https://zips.z.cash/protocol/nu5.pdf#pallasandvesta>
impl TryFrom<[u8; 32]> for ValueCommitment {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Affine::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid pallas::Affine value")
        }
    }
}

impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 32]>::from(*self)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Self::try_from(reader.read_32_bytes()?).map_err(SerializationError::Parse)
    }
}

#[cfg(test)]
mod tests {

    use std::ops::Neg;

    use group::Group;

    use super::*;

    #[test]
    fn add() {
        let _init_guard = zebra_test::init();

        let identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(pallas::Affine::generator());

        assert_eq!(identity + g, g);
    }

    #[test]
    fn add_assign() {
        let _init_guard = zebra_test::init();

        let mut identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(pallas::Affine::generator());

        identity += g;
        let new_g = identity;

        assert_eq!(new_g, g);
    }

    #[test]
    fn sub() {
        let _init_guard = zebra_test::init();

        let g_point = pallas::Affine::generator();

        let identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(g_point);

        assert_eq!(identity - g, ValueCommitment(g_point.neg()));
    }

    #[test]
    fn sub_assign() {
        let _init_guard = zebra_test::init();

        let g_point = pallas::Affine::generator();

        let mut identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(g_point);

        identity -= g;
        let new_g = identity;

        assert_eq!(new_g, ValueCommitment(g_point.neg()));
    }

    #[test]
    fn sum() {
        let _init_guard = zebra_test::init();

        let g_point = pallas::Affine::generator();

        let g = ValueCommitment(g_point);
        let other_g = ValueCommitment(g_point);

        let sum: ValueCommitment = vec![g, other_g].into_iter().sum();

        let doubled_g = ValueCommitment(g_point.to_curve().double().into());

        assert_eq!(sum, doubled_g);
    }
}
