//! Note encryption types.
mod memo;
pub mod sapling;
pub mod sprout;

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NoteCommitmentRandomness(pub [u8; 32]);

impl AsRef<[u8]> for NoteCommitmentRandomness {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
