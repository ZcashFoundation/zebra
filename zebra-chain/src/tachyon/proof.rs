//! Tachyon proofs for transaction and block-level verification.
//!
//! Tachyon uses a two-level proof system:
//! 1. Transaction-level proofs: Created by wallets, prove individual tx validity
//! 2. Block-level proofs: Aggregated by block builders using Ragu PCD
//!
//! This enables efficient verification - validators check one proof per block
//! instead of one proof per transaction.

use std::io;

use crate::serialization::{
    zcash_serialize_bytes, zcash_serialize_bytes_external_count, SerializationError,
    ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
};

/// Transaction-level Tachyon proof.
///
/// This is an intermediate representation created by wallets that proves
/// a single transaction's validity. At block creation time, these proofs
/// are aggregated into a single `RaguBlockProof` using the Ragu PCD library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionProof {
    /// The proof bytes (Ragu step proof).
    ///
    /// The exact format depends on the Ragu circuit being used.
    bytes: Vec<u8>,
}

impl TransactionProof {
    /// Minimum proof size in bytes.
    ///
    /// A zero-length proof may be valid for certain edge cases (e.g., pure
    /// transparent transactions with Tachyon outputs but no spends).
    pub const MIN_SIZE: usize = 0;

    /// Maximum proof size in bytes.
    ///
    /// This is a conservative upper bound. Actual proofs should be much smaller.
    pub const MAX_SIZE: usize = 8192;

    /// Create a new transaction proof from bytes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, &'static str> {
        if bytes.len() > Self::MAX_SIZE {
            return Err("Transaction proof too large");
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
}

impl ZcashSerialize for TransactionProof {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.bytes, writer)
    }
}

impl ZcashDeserialize for TransactionProof {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        let bytes: Vec<u8> = reader.zcash_deserialize_into()?;
        if bytes.len() > Self::MAX_SIZE {
            return Err(SerializationError::Parse("Transaction proof too large"));
        }
        Ok(Self { bytes })
    }
}

/// Block-level aggregated Ragu proof.
///
/// This is the result of aggregating all transaction-level proofs in a block
/// using the Ragu PCD library. Validators verify this single proof instead
/// of verifying each transaction proof individually.
///
/// The aggregation process:
/// 1. Collect all `TransactionProof`s from Tachyon transactions in block
/// 2. Build a binary tree of proofs
/// 3. Use Ragu `fuse` operation to combine proofs at each level
/// 4. Final root proof is the `RaguBlockProof`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaguBlockProof {
    /// The aggregated proof bytes.
    bytes: Vec<u8>,
}

impl RaguBlockProof {
    /// Maximum block proof size in bytes.
    ///
    /// Block proofs may be larger than transaction proofs due to accumulation
    /// of public inputs, but should still be bounded.
    pub const MAX_SIZE: usize = 16384;

    /// Create a new block proof from bytes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, &'static str> {
        if bytes.len() > Self::MAX_SIZE {
            return Err("Block proof too large");
        }
        Ok(Self { bytes })
    }

    /// Create an empty proof (for blocks with no Tachyon transactions).
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

impl ZcashSerialize for RaguBlockProof {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.bytes, writer)
    }
}

impl ZcashDeserialize for RaguBlockProof {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        let bytes: Vec<u8> = reader.zcash_deserialize_into()?;
        if bytes.len() > Self::MAX_SIZE {
            return Err(SerializationError::Parse("Block proof too large"));
        }
        Ok(Self { bytes })
    }
}

/// Serialization helper for proofs with externally tracked count.
///
/// Used when the proof size is serialized separately from the proof bytes.
impl TransactionProof {
    /// Serialize proof bytes without length prefix.
    pub fn zcash_serialize_external_count<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes_external_count(&self.bytes, writer)
    }

    /// Deserialize proof bytes with externally provided length.
    pub fn zcash_deserialize_external_count<R: io::Read>(
        mut reader: R,
        len: usize,
    ) -> Result<Self, SerializationError> {
        if len > Self::MAX_SIZE {
            return Err(SerializationError::Parse("Transaction proof too large"));
        }
        let mut bytes = vec![0u8; len];
        reader.read_exact(&mut bytes)?;
        Ok(Self { bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_proof_roundtrip() {
        let _init_guard = zebra_test::init();

        let proof = TransactionProof::new(vec![1, 2, 3, 4, 5]).unwrap();

        let mut bytes = Vec::new();
        proof.zcash_serialize(&mut bytes).unwrap();

        let proof2 = TransactionProof::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(proof, proof2);
    }

    #[test]
    fn block_proof_empty() {
        let _init_guard = zebra_test::init();

        let proof = RaguBlockProof::empty();
        assert!(proof.is_empty());
        assert_eq!(proof.size(), 0);
    }
}
