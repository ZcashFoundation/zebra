//! Note and value commitments.

#[cfg(test)]
mod test_vectors;

pub mod pedersen_hashes;

use std::{convert::TryFrom, fmt, io};

use bitvec::prelude::*;
use rand_core::{CryptoRng, RngCore};

use crate::{
    amount::{Amount, NonNegative},
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

use super::keys::{find_group_hash, Diversifier, TransmissionKey};

use pedersen_hashes::*;

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CommitmentRandomness(jubjub::Fr);

/// Note commitments for the output notes.
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
pub struct NoteCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for NoteCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NoteCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl Eq for NoteCommitment {}

impl From<jubjub::ExtendedPoint> for NoteCommitment {
    fn from(extended_point: jubjub::ExtendedPoint) -> Self {
        Self(jubjub::AffinePoint::from(extended_point))
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
        let possible_point = jubjub::AffinePoint::from_bytes(bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid jubjub::AffinePoint value")
        }
    }
}

impl NoteCommitment {
    /// Generate a new _NoteCommitment_ and the randomness used to create it.
    ///
    /// We return the randomness because it is needed to construct a _Note_,
    /// before it is encrypted as part of an _Output Description_. This is a
    /// higher level function that calls `NoteCommit^Sapling_rcm` internally.
    ///
    /// NoteCommit^Sapling_rcm (g*_d , pk*_d , v) :=
    ///   WindowedPedersenCommit_rcm([1; 6] || I2LEBSP_64(v) || g*_d || pk*_d)
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

        // Jubjub repr_J canonical byte encoding
        // https://zips.z.cash/protocol/protocol.pdf#jubjub
        //
        // The `TryFrom<Diversifier>` impls for the `jubjub::*Point`s handles
        // calling `DiversifyHash` implicitly.
        let g_d_bytes: [u8; 32];
        if let Ok(g_d) = jubjub::AffinePoint::try_from(diversifier) {
            g_d_bytes = g_d.to_bytes();
        } else {
            return None;
        }

        let pk_d_bytes = <[u8; 32]>::from(transmission_key);
        let v_bytes = value.to_bytes();

        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&g_d_bytes[..]));
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&pk_d_bytes[..]));
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&v_bytes[..]));

        let rcm = CommitmentRandomness(generate_trapdoor(csprng));

        Some((
            rcm,
            NoteCommitment::from(windowed_pedersen_commitment(rcm.0, &s)),
        ))
    }

    /// Hash Extractor for Jubjub (?)
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concreteextractorjubjub
    pub fn extract_u(&self) -> jubjub::Fq {
        self.0.get_u()
    }
}

/// A Homomorphic Pedersen commitment to the value of a note, used in Spend and
/// Output Descriptions.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
pub struct ValueCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl<'a> std::ops::Add<&'a ValueCommitment> for ValueCommitment {
    type Output = Self;

    fn add(self, rhs: &'a ValueCommitment) -> Self::Output {
        self + *rhs
    }
}

impl std::ops::Add<ValueCommitment> for ValueCommitment {
    type Output = Self;

    fn add(self, rhs: ValueCommitment) -> Self::Output {
        let value = self.0.to_extended() + rhs.0.to_extended();
        ValueCommitment(value.into())
    }
}

impl std::ops::AddAssign<ValueCommitment> for ValueCommitment {
    fn add_assign(&mut self, rhs: ValueCommitment) {
        *self = *self + rhs
    }
}

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ValueCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<jubjub::ExtendedPoint> for ValueCommitment {
    fn from(extended_point: jubjub::ExtendedPoint) -> Self {
        Self(jubjub::AffinePoint::from(extended_point))
    }
}

impl Eq for ValueCommitment {}

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
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
        ValueCommitment((self.0.to_extended() - rhs.0.to_extended()).into())
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
            ValueCommitment(jubjub::AffinePoint::identity()),
            std::ops::Add::add,
        )
    }
}

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
impl TryFrom<[u8; 32]> for ValueCommitment {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = jubjub::AffinePoint::from_bytes(bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid jubjub::AffinePoint value")
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
    /// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
    #[allow(non_snake_case)]
    pub fn new(rcv: jubjub::Fr, value: Amount) -> Self {
        let v = jubjub::Fr::from(value);

        // TODO: These generator points can be generated once somewhere else to
        // avoid having to recompute them on every new commitment.
        let V = find_group_hash(*b"Zcash_cv", b"v");
        let R = find_group_hash(*b"Zcash_cv", b"r");

        Self::from(V * v + R * rcv)
    }
}

#[cfg(test)]
mod tests {

    use std::ops::Neg;

    use super::*;

    #[test]
    fn pedersen_hash_to_point_test_vectors() {
        const D: [u8; 8] = *b"Zcash_PH";

        for test_vector in test_vectors::TEST_VECTORS.iter() {
            let result = jubjub::AffinePoint::from(pedersen_hash_to_point(
                D,
                &test_vector.input_bits.clone(),
            ));

            assert_eq!(result, test_vector.output_point);
        }
    }

    #[test]
    fn add() {
        let identity = ValueCommitment(jubjub::AffinePoint::identity());

        let g = ValueCommitment(jubjub::AffinePoint::from_raw_unchecked(
            jubjub::Fq::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            jubjub::Fq::from_raw([
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
        let mut identity = ValueCommitment(jubjub::AffinePoint::identity());

        let g = ValueCommitment(jubjub::AffinePoint::from_raw_unchecked(
            jubjub::Fq::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            jubjub::Fq::from_raw([
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
        let g_point = jubjub::AffinePoint::from_raw_unchecked(
            jubjub::Fq::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            jubjub::Fq::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        );

        let identity = ValueCommitment(jubjub::AffinePoint::identity());

        let g = ValueCommitment(g_point);

        assert_eq!(identity - g, ValueCommitment(g_point.neg()));
    }

    #[test]
    fn sub_assign() {
        let g_point = jubjub::AffinePoint::from_raw_unchecked(
            jubjub::Fq::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            jubjub::Fq::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        );

        let mut identity = ValueCommitment(jubjub::AffinePoint::identity());

        let g = ValueCommitment(g_point);

        identity -= g;
        let new_g = identity;

        assert_eq!(new_g, ValueCommitment(g_point.neg()));
    }

    #[test]
    fn sum() {
        let g_point = jubjub::AffinePoint::from_raw_unchecked(
            jubjub::Fq::from_raw([
                0xe4b3_d35d_f1a7_adfe,
                0xcaf5_5d1b_29bf_81af,
                0x8b0f_03dd_d60a_8187,
                0x62ed_cbb8_bf37_87c8,
            ]),
            jubjub::Fq::from_raw([
                0x0000_0000_0000_000b,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
                0x0000_0000_0000_0000,
            ]),
        );

        let g = ValueCommitment(g_point);
        let other_g = ValueCommitment(g_point);

        let sum: ValueCommitment = vec![g, other_g].into_iter().sum();

        let doubled_g = ValueCommitment(g_point.to_extended().double().into());

        assert_eq!(sum, doubled_g);
    }
}
