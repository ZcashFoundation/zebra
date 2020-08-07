//! Sapling note and value commitments and types.

#[cfg(test)]
mod arbitrary;
#[cfg(test)]
mod test_vectors;

pub mod pedersen_hashes;

use std::fmt;

use bitvec::prelude::*;
use rand_core::{CryptoRng, RngCore};

use crate::{
    keys::sapling::{find_group_hash, Diversifier, TransmissionKey},
    serde_helpers,
    types::amount::{Amount, NonNegative},
};

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
        //
        // The `From<Diversifier>` impls for the `jubjub::*Point`s handles
        // calling `DiversifyHash` implicitly.
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
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
pub struct ValueCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ValueCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
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

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
impl From<ValueCommitment> for [u8; 32] {
    fn from(cm: ValueCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

impl ValueCommitment {
    /// Generate a new _ValueCommitment_.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
    #[allow(non_snake_case)]
    pub fn new<T>(csprng: &mut T, value: Amount<NonNegative>) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let v = jubjub::Fr::from(value);
        let rcv = generate_trapdoor(csprng);

        let V = find_group_hash(*b"Zcash_cv", b"v");
        let R = find_group_hash(*b"Zcash_cv", b"r");

        Self::from(V * v + R * rcv)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::commitments::sapling::test_vectors::TEST_VECTORS;

    #[test]
    fn pedersen_hash_to_point_test_vectors() {
        const D: [u8; 8] = *b"Zcash_PH";

        for test_vector in TEST_VECTORS.iter() {
            let result = jubjub::AffinePoint::from(pedersen_hash_to_point(
                D,
                &test_vector.input_bits.clone(),
            ));

            assert_eq!(result, test_vector.output_point);
        }
    }
}
