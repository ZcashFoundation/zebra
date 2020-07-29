//! Sapling note and value commitments and types.

#![allow(clippy::unit_arg)]

#[cfg(test)]
mod arbitrary;

use std::{fmt, io};

use bitvec::prelude::*;
use rand_core::{CryptoRng, RngCore};

use crate::{
    keys::sapling::{find_group_hash, Diversifier, TransmissionKey},
    serde_helpers,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    types::amount::{Amount, NonNegative},
};

/// Generates a random scalar from the scalar field \mathbb{F}_r_𝕁.
///
/// The prime order subgroup 𝕁^(r) is the order-r_𝕁 subgroup of 𝕁 after the
/// Edwards cofactor h_𝕁 = 8 is factored out. This function is useful when
/// generating the uniform distribution on \mathbb{F}_r_𝕁 needed for Sapling
/// commitment schemes' trapdoor generators.
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

/// "...an algebraic hash function with collision resistance (for fixed input
/// length) derived from assumed hardness of the Discrete Logarithm Problem on
/// the Jubjub curve."
///
/// PedersenHash is used in the definitions of Pedersen commitments (§
/// 5.4.7.2‘Windowed Pedersen commitments’), and of the Pedersen hash for the
/// Sapling incremental Merkle tree (§ 5.4.1.3 ‘MerkleCRH^Sapling Hash
/// Function’).
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
pub fn pedersen_hash_to_point(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> jubjub::ExtendedPoint {
    // Expects i to be 0-indexed
    fn I_i(domain: [u8; 8], i: usize) -> jubjub::ExtendedPoint {
        find_group_hash(domain, &i.to_le_bytes())
    }

    // ⟨Mᵢ⟩
    fn M_i(segment: &BitSlice<Lsb0, u8>) -> jubjub::Fr {
        let mut m_i = [0u8; 32];

        for (j, chunk) in segment.chunks(3).enumerate() {
            let mut data = [0u8; 3];
            let bits = data.bits_mut::<Lsb0>();
            bits.copy_from_slice(chunk);

            let enc_m_j = (1 - (2 * bits[2] as u8)) * (1 + (bits[0] as u8) + (2 * bits[1] as u8));

            m_i[0] += enc_m_j * (1 << (4 * j))
        }

        jubjub::Fr::from_bytes(&m_i).unwrap()
    }

    let mut result = jubjub::ExtendedPoint::identity();

    // Split M into n segments of 3 * c bits, where c = 63, padding the last
    // segment with zeros.
    //
    // https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
    for (i, segment) in M.chunks(189).enumerate() {
        result += I_i(domain, i) * M_i(&segment)
    }

    result
}

/// Mixing Pedersen Hash Function
///
/// Used to compute ρ from a note commitment and its position in the note
/// commitment tree.  It takes as input a Pedersen commitment P, and hashes it
/// with another input x.
///
/// MixingPedersenHash(P, x) := P + [x]FindGroupHash^J^(r)(“Zcash_J_”, “”)
///
/// https://zips.z.cash/protocol/protocol.pdf#concretemixinghash
#[allow(non_snake_case)]
pub fn mixing_pedersen_hash(P: jubjub::ExtendedPoint, x: jubjub::Fr) -> jubjub::ExtendedPoint {
    const J: [u8; 8] = *b"Zcash_J_";

    P + find_group_hash(J, b"") * x
}

/// Construct a 'windowed' Pedersen commitment by reusing a Perderson hash
/// constructon, and adding a randomized point on the Jubjub curve.
///
/// WindowedPedersenCommit_r (s) := \
///   PedersenHashToPoint(“Zcash_PH”, s) + [r]FindGroupHash^J^(r)(“Zcash_PH”, “r”)
///
/// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
pub fn windowed_pedersen_commitment(r: jubjub::Fr, s: &BitVec<Lsb0, u8>) -> jubjub::ExtendedPoint {
    const D: [u8; 8] = *b"Zcash_PH";

    pedersen_hash_to_point(D, &s) + find_group_hash(D, b"r") * r
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

impl From<NoteCommitment> for [u8; 32] {
    fn from(cm: NoteCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

impl Eq for NoteCommitment {}

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
    /// Generate a new _NoteCommitment_ and the randomness used to create it.
    ///
    /// We return the randomness because it is needed to construct a _Note_,
    /// before it is encrypted as part of an _Output Description_.
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
    ) -> (CommitmentRandomness, Self)
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

        let rcm = CommitmentRandomness(generate_trapdoor(csprng));

        (
            rcm,
            NoteCommitment::from(windowed_pedersen_commitment(rcm.0, &s)),
        )
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
#[derive(Clone, Deserialize, PartialEq, Serialize)]
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
        let v = jubjub::Fr::from_bytes(&value_bytes).unwrap();
        let rcv = generate_trapdoor(csprng);

        let V = find_group_hash(*b"Zcash_cv", b"v");
        let R = find_group_hash(*b"Zcash_cv", b"r");

        Self::from(V * v + R * rcv)
    }
}
