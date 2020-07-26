//! Sapling note and value commitments

use std::{fmt, io};

use bitvec::prelude::*;
use rand_core::{CryptoRng, RngCore};

use crate::{
    keys::sapling::{find_group_hash, Diversifier, TransmissionKey},
    serde_helpers,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    types::amount::{Amount, NonNegative},
};

// TODO: replace with reference to redjubjub or jubjub when merged and
// exported.
type Scalar = jubjub::Fr;

/// "...an algebraic hash function with collision resistance (for
/// fixed input length) derived from assumed hardness of the Discrete
/// Logarithm Problem on the Jubjub curve."
///
/// PedersenHash is used in the definitions of Pedersen commitments (§
/// 5.4.7.2‘Windowed Pedersen commitments’), and of the Pedersen hash
/// for the Sapling incremental Merkle tree (§
/// 5.4.1.3 ‘MerkleCRH^Sapling Hash Function’).
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
pub fn pedersen_hash_to_point(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> jubjub::ExtendedPoint {
    // Expects i to be 0-indexed
    fn I_i(domain: [u8; 8], i: usize) -> jubjub::ExtendedPoint {
        find_group_hash(domain, &i.to_le_bytes())
    }

    // ⟨Mᵢ⟩
    fn M_i(segment: &BitSlice<Lsb0, u8>) -> Scalar {
        let mut m_i = [0u8; 32];

        for (j, chunk) in segment.chunks(3).enumerate() {
            let mut data = [0u8; 3];
            let bits = data.bits_mut::<Lsb0>();
            bits.copy_from_slice(chunk);

            let enc_m_j = (1 - (2 * bits[2] as u8)) * (1 + (bits[0] as u8) + (2 * bits[1] as u8));

            m_i[0] += enc_m_j * (1 << (4 * j))
        }

        Scalar::from_bytes(&m_i).unwrap()
    }

    let mut result = jubjub::ExtendedPoint::identity();

    // Split M into n segments of 3 * c bits, where c = 63, padding
    // the last segment with zeros.
    //
    // https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
    for (i, segment) in M.chunks(189).enumerate() {
        result += I_i(domain, i) * M_i(&segment)
    }

    result
}

/// Construct a “windowed” Pedersen commitment by reusing a Perderson
/// hash constructon, and adding a randomized point on the Jubjub
/// curve.
///
/// WindowedPedersenCommit_r (s) := \
///   PedersenHashToPoint(“Zcash_PH”, s) + [r]FindGroupHash^J^(r)(“Zcash_PH”, “r”)
///
/// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
pub fn windowed_pedersen_commitment_r<T>(
    csprng: &mut T,
    s: &BitVec<Lsb0, u8>,
) -> jubjub::ExtendedPoint
where
    T: RngCore + CryptoRng,
{
    const D: [u8; 8] = *b"Zcash_PH";

    let mut r_bytes = [0u8; 32];
    csprng.fill_bytes(&mut r_bytes);
    let r = Scalar::from_bytes(&r_bytes).unwrap();

    pedersen_hash_to_point(D, &s) + find_group_hash(D, b"r") * r
}

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CommitmentRandomness(redjubjub::Randomizer);

/// Note commitments for the output notes.
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
//#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct NoteCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for NoteCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NoteCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for NoteCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl From<jubjub::ExtendedPoint> for NoteCommitment {
    fn from(extended_point: jubjub::ExtendedPoint) -> Self {
        Self(jubjub::AffinePoint::from(extended_point))
    }
}

impl Eq for NoteCommitment {}

impl From<NoteCommitment> for [u8; 32] {
    fn from(cm: NoteCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

impl ZcashSerialize for NoteCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
        Ok(())
    }
}

impl ZcashDeserialize for NoteCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
        ))
    }
}

impl NoteCommitment {
    /// Generate a new _NoteCommitment_.
    ///
    /// NoteCommit^Sapling_rcm (g*_d , pk*_d , v) := \
    ///   WindowedPedersenCommit_rcm([1; 6] || I2LEBSP_64(v) || g*_d || pk*_d)
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
    #[allow(non_snake_case)]
    pub fn new<T>(
        csprng: &mut T,
        diversifier: Diversifier,
        transmission_key: TransmissionKey,
        value: Amount<NonNegative>,
    ) -> Self
    where
        T: RngCore + CryptoRng,
    {
        // s as in the argument name for WindowedPedersenCommit_r(s)
        let mut s: BitVec<Lsb0, u8> = BitVec::new();

        // Prefix
        s.append(&mut bitvec![1; 6]);

        // Jubjub repr_J canonical byte encoding
        // https://zips.z.cash/protocol/protocol.pdf#jubjub
        let g_d_bytes = jubjub::AffinePoint::from(diversifier).to_bytes();
        let pk_d_bytes = <[u8; 32]>::from(transmission_key);
        let v_bytes = value.to_bytes();

        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&g_d_bytes[..]));
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&pk_d_bytes[..]));
        s.append(&mut BitVec::<Lsb0, u8>::from_slice(&v_bytes[..]));

        Self::from(windowed_pedersen_commitment_r(csprng, &s))
    }

    /// Hash Extractor for Jubjub (?)
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concreteextractorjubjub
    pub fn extract_u(self) -> jubjub::Fq {
        self.0.get_u()
    }
}

/// A Homomorphic Pedersen commitment to the value of a note, used in
/// Spend and Output Descriptions.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
#[derive(Clone, Deserialize, PartialEq, Serialize)]
//#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct ValueCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ValueCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for ValueCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl From<jubjub::ExtendedPoint> for ValueCommitment {
    fn from(extended_point: jubjub::ExtendedPoint) -> Self {
        Self(jubjub::AffinePoint::from(extended_point))
    }
}

impl Eq for ValueCommitment {}

impl From<ValueCommitment> for [u8; 32] {
    fn from(cm: ValueCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
        Ok(())
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
        ))
    }
}

impl ValueCommitment {
    /// Generate a new _ValueCommitment_.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
    // TODO: accept an Amount instead?
    #[allow(non_snake_case)]
    pub fn new<T>(csprng: &mut T, value_bytes: [u8; 32]) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let v = Scalar::from_bytes(&value_bytes).unwrap();

        let mut rcv_bytes = [0u8; 32];
        csprng.fill_bytes(&mut rcv_bytes);
        let rcv = Scalar::from_bytes(&rcv_bytes).unwrap();

        let V = find_group_hash(*b"Zcash_cv", b"v");
        let R = find_group_hash(*b"Zcash_cv", b"r");

        Self::from(V * v + R * rcv)
    }
}
