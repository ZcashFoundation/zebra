//! Tachyon proofs for aggregate transaction verification.
//!
//! Tachyon uses an aggregate proof model:
//! - **Aggregate transactions** contain an `AggregateProof` covering multiple tachyon txs
//! - **Regular tachyon transactions** reference an aggregate by transaction hash
//!
//! Multiple aggregate transactions may exist per block, and regular tachyon
//! transactions specify which aggregate covers them.

use std::io;

use crate::serialization::{
    zcash_serialize_bytes, SerializationError, ZcashDeserialize, ZcashDeserializeInto,
    ZcashSerialize,
};

/// Aggregated Ragu proof covering multiple tachyon transactions.
///
/// This proof is created by aggregating proofs for multiple tachyon transactions
/// using the Ragu PCD library. It lives in a special "aggregate transaction"
/// that other tachyon transactions reference.
///
/// The aggregation process:
/// 1. Collect proofs from tachyon transactions to be covered
/// 2. Build a binary tree of proofs
/// 3. Use Ragu `fuse` operation to combine proofs at each level
/// 4. Final root proof becomes the `AggregateProof`
///
/// Multiple aggregate transactions may exist per block, each covering
/// a different set of tachyon transactions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateProof {
    /// The aggregated proof bytes.
    bytes: Vec<u8>,
}

impl AggregateProof {
    /// Maximum aggregate proof size in bytes.
    ///
    /// Aggregate proofs may be larger than individual proofs due to
    /// accumulation of public inputs, but should still be bounded.
    pub const MAX_SIZE: usize = 16384;

    /// Create a new aggregate proof from bytes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, &'static str> {
        if bytes.len() > Self::MAX_SIZE {
            return Err("Aggregate proof too large");
        }
        Ok(Self { bytes })
    }

    /// Create an empty proof (for testing or placeholder purposes).
    pub fn empty() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Get the proof bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Get the size of the proof.
    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    /// Check if this is an empty proof.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl ZcashSerialize for AggregateProof {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.bytes, writer)
    }
}

impl ZcashDeserialize for AggregateProof {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        let bytes: Vec<u8> = reader.zcash_deserialize_into()?;
        if bytes.len() > Self::MAX_SIZE {
            return Err(SerializationError::Parse("Aggregate proof too large"));
        }
        Ok(Self { bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_proof_roundtrip() {
        let _init_guard = zebra_test::init();

        let proof = AggregateProof::new(vec![1, 2, 3, 4, 5]).unwrap();

        let mut bytes = Vec::new();
        proof.zcash_serialize(&mut bytes).unwrap();

        let proof2 = AggregateProof::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(proof, proof2);
    }

    #[test]
    fn aggregate_proof_empty() {
        let _init_guard = zebra_test::init();

        let proof = AggregateProof::empty();
        assert!(proof.is_empty());
        assert_eq!(proof.size(), 0);
    }
}
