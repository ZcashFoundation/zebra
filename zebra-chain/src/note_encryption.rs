//! Note encryption types.
mod memo;
mod sapling;
mod sprout;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Diversifier(pub [u8; 11]);

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NoteCommitmentRandomness(pub [u8; 32]);
