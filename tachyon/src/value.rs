//! Tachyon value types.

/// A value commitment for a Tachyon action.
///
/// Commits to the value being transferred in an action without revealing it.
/// Used in value balance verification within the Ragu proof.
///
/// This is a stub type for future implementation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValueCommitment([u8; 32]);

impl ValueCommitment {
    /// Creates a new value commitment from bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        ValueCommitment(bytes)
    }

    /// Returns the byte representation of this value commitment.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for ValueCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        ValueCommitment(bytes)
    }
}

impl From<ValueCommitment> for [u8; 32] {
    fn from(commitment: ValueCommitment) -> Self {
        commitment.0
    }
}

impl AsRef<[u8; 32]> for ValueCommitment {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}
