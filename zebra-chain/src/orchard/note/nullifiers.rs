#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use halo2::pasta::pallas;

use super::super::{
    commitment::NoteCommitment, keys::NullifierDerivingKey, sinsemilla::*, tree::Position,
};

/// Orchard ixing Pedersen hash Function
///
/// Used to compute ρ from a note commitment and its position in the note
/// commitment tree.  It takes as input a Pedersen commitment P, and hashes it
/// with another input x.
///
/// MixingPedersenHash(P, x) := P + [x]GroupHash^P^(r)(“Zcash_P_”, “”)
///
/// https://zips.z.cash/protocol/protocol.pdf#concretemixinghash
// TODO: I'M EXTRAPOLATING HERE, DOUBLE CHECK THE SPEC WHEN FINALIZED
#[allow(non_snake_case)]
pub fn mixing_pedersen_hash(P: pallas::Point, x: pallas::Scalar) -> pallas::Point {
    P + pallas_group_hash(*b"Zcash_P_", b"") * x
}

/// A cryptographic permutation, defined in [poseidonhash].
///
/// PoseidonHash(x, y) = f([x, y, 0])_1 (using 1-based indexing).
///
/// [poseidonhash]: https://zips.z.cash/protocol/nu5.pdf#poseidonhash
fn poseidon_hash(x: pallas::Base, y: pallas::Base) -> pallas::Base {
    unimplemented!()
}

/// Derive the nullifier for a Orchard note.
///
/// Instantiated using the PoseidonHash hash function defined in [§5.4.1.10
/// ‘PoseidonHash Function’][poseidon]
///
/// PRF^nfOrchard(nk*, ρ*) := PoseidonHash(nk*, ρ*)
///
/// [concreteprfs]: https://zips.z.cash/protocol/protocol.pdf#concreteprfs
/// [poseidonhash]: https://zips.z.cash/protocol/nu5.pdf#poseidonhash
fn prf_nf(nk: [u8; 32], rho: [u8; 32]) -> [u8; 32] {
    poseidon_hash(nk, rho)
}

/// A Nullifier for Orchard transactions
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Nullifier(pub [u8; 32]);

impl From<[u8; 32]> for Nullifier {
    fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
}

impl<'a> From<(NoteCommitment, Position, &'a NullifierDerivingKey)> for Nullifier {
    /// Derive a `Nullifier` for an Orchard _note_.
    ///
    /// For a _note_, the _nullifier_ is derived as PRF^nfOrchard_nk(ρ*), where
    /// k is a representation of the _nullifier deriving key_ associated with
    /// the _note_ and ρ = repr_P(ρ).
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#commitmentsandnullifiers
    fn from((cm, pos, nk): (NoteCommitment, Position, &'a NullifierDerivingKey)) -> Self {
        let rho = pallas::Affine::from(mixing_pedersen_hash(cm.0.into(), pos.0.into()));

        Nullifier(prf_nf(nk.into(), rho.to_bytes()))
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        n.0
    }
}
