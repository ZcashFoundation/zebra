#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use halo2::{arithmetic::FieldExt, pasta::pallas};

use crate::serialization::{serde_helpers, SerializationError};

use super::super::{
    commitment::NoteCommitment, keys::NullifierDerivingKey, note::Note, sinsemilla::*,
};

use std::{
    convert::TryFrom,
    hash::{Hash, Hasher},
};

/// A cryptographic permutation, defined in [poseidonhash].
///
/// PoseidonHash(x, y) = f([x, y, 0])_1 (using 1-based indexing).
///
/// [poseidonhash]: https://zips.z.cash/protocol/nu5.pdf#poseidonhash
fn poseidon_hash(_x: pallas::Base, _y: pallas::Base) -> pallas::Base {
    // TODO: implement: #2064
    unimplemented!()
}

/// Used as part of deriving the _nullifier_ for a Orchard _note_.
///
/// PRF^nfOrchard: F_𝑞P × F_𝑞P → F_𝑞P
///
/// Instantiated using the PoseidonHash hash function defined in [§5.4.1.10
/// ‘PoseidonHash Function’][poseidon]:
///
/// PRF^nfOrchard(nk*, ρ*) := PoseidonHash(nk*, ρ*)
///
/// [concreteprfs]: https://zips.z.cash/protocol/nu5.pdf#concreteprfs
/// [poseidonhash]: https://zips.z.cash/protocol/nu5.pdf#poseidonhash
fn prf_nf(nk: pallas::Base, rho: pallas::Base) -> pallas::Base {
    poseidon_hash(nk, rho)
}

/// A Nullifier for Orchard transactions
#[derive(Clone, Copy, Debug, Eq, Serialize, Deserialize)]
pub struct Nullifier(#[serde(with = "serde_helpers::Base")] pallas::Base);

impl Hash for Nullifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bytes().hash(state);
    }
}

impl TryFrom<[u8; 32]> for Nullifier {
    type Error = SerializationError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Base::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base value for orchard Nullifier",
            ))
        }
    }
}

impl PartialEq for Nullifier {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl From<(NullifierDerivingKey, Note, NoteCommitment)> for Nullifier {
    /// Derive a `Nullifier` for an Orchard _note_.
    ///
    /// nk is the _nullifier deriving key_ associated with the _note_; ρ and ψ
    /// are part of the _note_; and cm is the _note commitment_.
    ///
    /// DeriveNullifier_nk(ρ, ψ, cm) = Extract_P(︀ [︀ (PRF^nfOrchard_nk(ρ) + ψ) mod q_P ]︀ K^Orchard + cm)︀
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#commitmentsandnullifiers
    #[allow(non_snake_case)]
    fn from((nk, note, cm): (NullifierDerivingKey, Note, NoteCommitment)) -> Self {
        // https://zips.z.cash/protocol/nu5.pdf#commitmentsandnullifiers
        let K = pallas_group_hash(b"z.cash:Orchard", b"K");

        // impl Add for pallas::Base reduces by the modulus (q_P)
        //
        // [︀ (PRF^nfOrchard_nk(ρ) + ψ) mod q_P ]︀ K^Orchard + cm
        let scalar =
            pallas::Scalar::from_bytes(&(prf_nf(nk.0, note.rho.0) + note.psi.0).to_bytes())
                .unwrap();

        // Basically a new-gen Pedersen hash?
        Nullifier(extract_p((K * scalar) + cm.0))
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        n.0.into()
    }
}
