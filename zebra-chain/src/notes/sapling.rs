#![allow(clippy::unit_arg)]
#![allow(dead_code)]

#[cfg(test)]
mod arbitrary;
mod ciphertexts;
mod commitments;
mod nullifiers;

use crate::{
    keys::sapling::{Diversifier, TransmissionKey},
    notes::memo::Memo,
    types::amount::{Amount, NonNegative},
};

pub use ciphertexts::{EncryptedCiphertext, OutCiphertext};
pub use commitments::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use nullifiers::Nullifier;

///
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
pub fn pedersen_hash_to_point() {}

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
pub struct Note {
    diversifier: Diversifier,
    transmission_key: TransmissionKey,
    value: Amount<NonNegative>,
    rcm: CommitmentRandomness,
}

impl Note {
    /// Construct a “windowed” Pedersen commitment by reusing a
    /// Perderson hash constructon, and adding a randomized point on
    /// the Jubjub curve.
    ///
    /// WindowedPedersenCommit_r (s) := \
    ///   PedersenHashToPoint(“Zcash_PH”, s) + [r]FindGroupHash^J^(r)∗(“Zcash_PH”, “r”)
    ///
    /// NoteCommit^Sapling_rcm (g*_d , pk*_d , v) := \
    ///   WindowedPedersenCommit_rcm([1; 6] || I2LEBSP_64(v) || g*_d || pk*_d)
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
    pub fn commit(&self) -> NoteCommitment {
        use rand_core::OsRng;

        NoteCommitment::new(
            &mut OsRng,
            self.diversifier,
            self.transmission_key,
            self.value,
        )
    }
}

/// The decrypted form of encrypted Sapling notes on the blockchain.
pub struct NotePlaintext {
    diversifier: Diversifier,
    value: Amount<NonNegative>,
    rcm: CommitmentRandomness,
    memo: Memo,
}
