//! Note and value commitments.

// #[cfg(test)]
// mod test_vectors;

use std::{convert::TryFrom, fmt, io};

use bitvec::prelude::*;
use group::{prime::PrimeCurveAffine, GroupEncoding};
use halo2::{
    arithmetic::{CurveAffine, FieldExt},
    pasta::pallas,
};

use rand_core::{CryptoRng, RngCore};

use crate::{
    amount::{Amount, NonNegative},
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

use super::{
    keys::{Diversifier, TransmissionKey},
    sinsemilla::*,
};

/// Generates a random scalar from the scalar field 𝔽_{q_P}.
///
/// https://zips.z.cash/protocol/nu5.pdf#pallasandvesta
pub fn generate_trapdoor<T>(csprng: &mut T) -> pallas::Scalar
where
    T: RngCore + CryptoRng,
{
    let mut bytes = [0u8; 64];
    csprng.fill_bytes(&mut bytes);
    // pallas::Scalar::from_bytes_wide() reduces the input modulo q_P under the hood.
    pallas::Scalar::from_bytes_wide(&bytes)
}

/// The randomness used in the Simsemilla Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CommitmentRandomness(pallas::Scalar);

/// Note commitments for the output notes.
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
pub struct NoteCommitment(#[serde(with = "serde_helpers::Affine")] pub pallas::Affine);

impl fmt::Debug for NoteCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // This will panic if the public key is the identity, which is bad news
        // bears.
        let (x, y) = self.0.get_xy().unwrap();

        f.debug_struct("NoteCommitment")
            .field("x", &hex::encode(x.to_bytes()))
            .field("y", &hex::encode(y.to_bytes()))
            .finish()
    }
}

impl Eq for NoteCommitment {}

impl From<pallas::Point> for NoteCommitment {
    fn from(projective_point: pallas::Point) -> Self {
        Self(pallas::Affine::from(projective_point))
    }
}

impl From<NoteCommitment> for [u8; 32] {
    fn from(cm: NoteCommitment) -> [u8; 32] {
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
            Err("Invalid pallas::Affine value")
        }
    }
}

impl NoteCommitment {
    /// Generate a new _NoteCommitment_ and the randomness used to create it.
    ///
    /// We return the randomness because it is needed to construct a _Note_,
    /// before it is encrypted as part of an output of an _Action_. This is a
    /// higher level function that calls `NoteCommit^Orchard_rcm` internally.
    ///
    /// NoteCommit^Orchard_rcm(repr_P(gd),repr_P(pkd), v, ρ, ψ) :=
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
    #[allow(non_snake_case)]
    pub fn new<T>(
        csprng: &mut T,
        diversifier: Diversifier,
        transmission_key: TransmissionKey,
        value: Amount<NonNegative>,
    ) -> Option<(CommitmentRandomness, Self)>
    where
        T: RngCore + CryptoRng,
    {
        // s as in the argument name for WindowedPedersenCommit_r(s)
        let mut s: BitVec<Lsb0, u8> = BitVec::new();

        // Prefix
        s.append(&mut bitvec![1; 6]);

        // The `TryFrom<Diversifier>` impls for the `pallas::*Point`s handles
        // calling `DiversifyHash` implicitly.
        let g_d_bytes: [u8; 32];
        if let Ok(g_d) = pallas::Affine::try_from(diversifier) {
            g_d_bytes = g_d.to_bytes();
        } else {
            return None;
        }

        let pk_d_bytes = <[u8; 32]>::from(transmission_key);
        let v_bytes = value.to_bytes();

        // g*d || pk*d || I2LEBSP64(v)
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&g_d_bytes[..]));
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&pk_d_bytes[..]));
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&v_bytes[..]));

        let rcm = CommitmentRandomness(generate_trapdoor(csprng));

        Some((
            rcm,
            NoteCommitment::from(sinsemilla_commit(rcm.0, b"z.cash:Orchard-NoteCommit", &s)),
        ))
    }

    /// Hash Extractor for Pallas
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concreteextractorpallas
    pub fn extract_x(&self) -> pallas::Base {
        match self.0.get_xy().into() {
            // If Some, it's not the identity.
            Some((x, _)) => x,
            _ => pallas::Base::zero(),
        }
    }
}

/// A homomorphic Pedersen commitment to the net value of a note, used in Action
/// descriptions.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
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
        // This will panic if the public key is the identity, which is bad news
        // bears.
        let (x, y) = self.0.get_xy().unwrap();

        f.debug_struct("ValueCommitment")
            .field("x", &hex::encode(x.to_bytes()))
            .field("y", &hex::encode(y.to_bytes()))
            .finish()
    }
}

impl From<pallas::Point> for ValueCommitment {
    fn from(projective_point: pallas::Point) -> Self {
        Self(pallas::Affine::from(projective_point))
    }
}

impl Eq for ValueCommitment {}

/// LEBS2OSP256(repr_P(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#pallasandvesta
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
/// https://zips.z.cash/protocol/protocol.pdf#pallasandvesta
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
        Self::try_from(reader.read_32_bytes()?).map_err(|e| SerializationError::Parse(e))
    }
}

impl ValueCommitment {
    /// Generate a new _ValueCommitment_.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
    pub fn randomized<T>(csprng: &mut T, value: Amount) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let rcv = generate_trapdoor(csprng);

        Self::new(rcv, value)
    }

    /// Generate a new _ValueCommitment_ from an existing _rcv_ on a _value_.
    ///
    /// ValueCommit^Orchard(v) :=
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
    #[allow(non_snake_case)]
    pub fn new(rcv: pallas::Scalar, value: Amount) -> Self {
        let v = pallas::Scalar::from(value);

        // TODO: These generator points can be generated once somewhere else to
        // avoid having to recompute them on every new commitment.
        let V = pallas_group_hash(b"z.cash:Orchard-cv", b"v");
        let R = pallas_group_hash(b"z.cash:Orchard-cv", b"r");

        Self::from(V * v + R * rcv)
    }
}

#[cfg(test)]
mod tests {

    use std::ops::Neg;

    use super::*;

    // #[test]
    // fn sinsemilla_hash_to_point_test_vectors() {
    //     zebra_test::init();

    //     const D: [u8; 8] = *b"Zcash_PH";

    //     for test_vector in test_vectors::TEST_VECTORS.iter() {
    //         let result =
    //             pallas::Affine::from(sinsemilla_hash_to_point(D, &test_vector.input_bits.clone()));

    //         assert_eq!(result, test_vector.output_point);
    //     }
    // }

    // TODO: these test vectors for ops are from Jubjub, replace with Pallas ones

    #[test]
    fn add() {
        zebra_test::init();

        let identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(pallas::Affine::from_raw_unchecked(
            pallas::Base::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            pallas::Base::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        ));

        assert_eq!(identity + g, g);
    }

    #[test]
    fn add_assign() {
        zebra_test::init();

        let mut identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(pallas::Affine::from_raw_unchecked(
            pallas::Base::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            pallas::Base::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        ));

        identity += g;
        let new_g = identity;

        assert_eq!(new_g, g);
    }

    #[test]
    fn sub() {
        zebra_test::init();

        let g_point = pallas::Affine::from_raw_unchecked(
            pallas::Base::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            pallas::Base::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        );

        let identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(g_point);

        assert_eq!(identity - g, ValueCommitment(g_point.neg()));
    }

    #[test]
    fn sub_assign() {
        zebra_test::init();

        let g_point = pallas::Affine::from_raw_unchecked(
            pallas::Base::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            pallas::Base::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        );

        let mut identity = ValueCommitment(pallas::Affine::identity());

        let g = ValueCommitment(g_point);

        identity -= g;
        let new_g = identity;

        assert_eq!(new_g, ValueCommitment(g_point.neg()));
    }

    #[test]
    fn sum() {
        zebra_test::init();

        let g_point = pallas::Affine::from_raw_unchecked(
            pallas::Base::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            pallas::Base::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        );

        let g = ValueCommitment(g_point);
        let other_g = ValueCommitment(g_point);

        let sum: ValueCommitment = vec![g, other_g].into_iter().sum();

        let doubled_g = ValueCommitment(g_point.into().double().into());

        assert_eq!(sum, doubled_g);
    }
}
