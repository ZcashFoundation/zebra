#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use std::io;

use crate::{
    commitments::sapling::{mixing_pedersen_hash, NoteCommitment},
    keys::sapling::NullifierDerivingKey,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    treestate::note_commitment_tree::Position,
};

/// Invokes Blake2s-256 as PRF^nfSapling to derive the nullifier for a
/// Sapling note.
///
/// PRF^nfSapling(ρ*) := BLAKE2s-256("Zcash_nf", nk* || ρ*)
///
/// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
fn prf_nf(nk: [u8; 32], rho: [u8; 32]) -> [u8; 32] {
    let hash = blake2s_simd::Params::new()
        .hash_length(32)
        .personal(b"Zcash_nf")
        .to_state()
        .update(&nk[..])
        .update(&rho[..])
        .finalize();

    *hash.as_array()
}

/// A Nullifier for Sapling transactions
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Nullifier([u8; 32]);

impl From<[u8; 32]> for Nullifier {
    fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
}

impl From<(NoteCommitment, Position, NullifierDerivingKey)> for Nullifier {
    fn from((cm, pos, nk): (NoteCommitment, Position, NullifierDerivingKey)) -> Self {
        let rho = jubjub::AffinePoint::from(mixing_pedersen_hash(cm.0.into(), pos.0.into()));

        Nullifier(prf_nf(nk.into(), rho.to_bytes()))
    }
}

impl ZcashDeserialize for Nullifier {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;

        Ok(Self(bytes))
    }
}

impl ZcashSerialize for Nullifier {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])
    }
}
