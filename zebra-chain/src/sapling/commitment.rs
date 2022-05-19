//! Note and value commitments.

#[cfg(test)]
mod test_vectors;

pub mod pedersen_hashes;

use std::{
    convert::{TryFrom, TryInto},
    fmt, io,
};

use bitvec::prelude::*;
use jubjub::ExtendedPoint;
use rand_core::{CryptoRng, RngCore};

use crate::{
    amount::{Amount, NonNegative},
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

use super::keys::{find_group_hash, Diversifier, TransmissionKey};

use pedersen_hashes::*;

/// Generates a random scalar from the scalar field 𝔽_{r_𝕁}.
///
/// The prime order subgroup 𝕁^(r) is the order-r_𝕁 subgroup of 𝕁 that consists
/// of the points whose order divides r. This function is useful when generating
/// the uniform distribution on 𝔽_{r_𝕁} needed for Sapling commitment schemes'
/// trapdoor generators.
///
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
pub fn generate_trapdoor<T>(csprng: &mut T) -> jubjub::Fr
where
    T: RngCore + CryptoRng,
{
    let mut bytes = [0u8; 64];
    csprng.fill_bytes(&mut bytes);
    // Fr::from_bytes_wide() reduces the input modulo r via Fr::from_u512()
    jubjub::Fr::from_bytes_wide(&bytes)
}

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
        let mut s: BitVec<u8, Lsb0> = BitVec::new();

        // Prefix
        s.append(&mut bitvec![1; 6]);

        // Jubjub repr_J canonical byte encoding
        // https://zips.z.cash/protocol/protocol.pdf#jubjub
        //
        // The `TryFrom<Diversifier>` impls for the `jubjub::*Point`s handles
        // calling `DiversifyHash` implicitly.

        let g_d_bytes: [u8; 32] = if let Ok(g_d) = jubjub::AffinePoint::try_from(diversifier) {
            g_d.to_bytes()
        } else {
            return None;
        };

        let pk_d_bytes = <[u8; 32]>::from(transmission_key);
        let v_bytes = value.to_bytes();

        s.extend(g_d_bytes);
        s.extend(pk_d_bytes);
        s.extend(v_bytes);

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

/// A Homomorphic Pedersen commitment to the value of a note.
///
/// This can be used as an intermediate value in some computations. For the
/// type actually stored in Spend and Output descriptions, see
/// [`NotSmallOrderValueCommitment`].
///
/// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
pub struct ValueCommitment(#[serde(with = "serde_helpers::AffinePoint")] jubjub::AffinePoint);

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
    /// Convert a Jubjub point into a ValueCommitment.
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
            let point = possible_point.unwrap();
            Ok(ExtendedPoint::from(point).into())
        } else {
            Err("Invalid jubjub::AffinePoint value")
        }
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

/// A Homomorphic Pedersen commitment to the value of a note, used in Spend and
/// Output descriptions.
///
/// Elements that are of small order are not allowed. This is a separate
/// consensus rule and not intrinsic of value commitments; which is why this
/// type exists.
///
/// This is denoted by `cv` in the specification.
///
/// https://zips.z.cash/protocol/protocol.pdf#spenddesc
/// https://zips.z.cash/protocol/protocol.pdf#outputdesc
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
pub struct NotSmallOrderValueCommitment(ValueCommitment);

impl TryFrom<ValueCommitment> for NotSmallOrderValueCommitment {
    type Error = &'static str;

    /// Convert a ValueCommitment into a NotSmallOrderValueCommitment.
    ///
    /// Returns an error if the point is of small order.
    ///
    /// # Consensus
    ///
    /// > cv and rk [MUST NOT be of small order][1], i.e. [h_J]cv MUST NOT be 𝒪_J
    /// > and [h_J]rk MUST NOT be 𝒪_J.
    ///
    /// > cv and epk [MUST NOT be of small order][2], i.e. [h_J]cv MUST NOT be 𝒪_J
    /// > and [ℎ_J]epk MUST NOT be 𝒪_J.
    ///
    /// [1]: https://zips.z.cash/protocol/protocol.pdf#spenddesc
    /// [2]: https://zips.z.cash/protocol/protocol.pdf#outputdesc
    fn try_from(value_commitment: ValueCommitment) -> Result<Self, Self::Error> {
        if value_commitment.0.is_small_order().into() {
            Err("jubjub::AffinePoint value for Sapling ValueCommitment is of small order")
        } else {
            Ok(Self(value_commitment))
        }
    }
}

impl TryFrom<jubjub::ExtendedPoint> for NotSmallOrderValueCommitment {
    type Error = &'static str;

    /// Convert a Jubjub point into a NotSmallOrderValueCommitment.
    fn try_from(extended_point: jubjub::ExtendedPoint) -> Result<Self, Self::Error> {
        ValueCommitment::from(extended_point).try_into()
    }
}

impl From<NotSmallOrderValueCommitment> for ValueCommitment {
    fn from(cv: NotSmallOrderValueCommitment) -> Self {
        cv.0
    }
}

impl From<NotSmallOrderValueCommitment> for jubjub::AffinePoint {
    fn from(cv: NotSmallOrderValueCommitment) -> Self {
        cv.0 .0
    }
}

impl ZcashSerialize for NotSmallOrderValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 32]>::from(self.0)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for NotSmallOrderValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let vc = ValueCommitment::try_from(reader.read_32_bytes()?)
            .map_err(SerializationError::Parse)?;
        vc.try_into().map_err(SerializationError::Parse)
    }
}

#[cfg(test)]
mod tests {

    use std::ops::Neg;

    use super::*;

    #[test]
    fn pedersen_hash_to_point_test_vectors() {
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
        zebra_test::init();

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
