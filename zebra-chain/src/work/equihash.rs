//! Equihash Solution and related items.

use std::{fmt, io};

use serde_big_array::BigArray;

use crate::{
    block::Header,
    serialization::{
        zcash_serialize_bytes, SerializationError, ZcashDeserialize, ZcashDeserializeInto,
        ZcashSerialize,
    },
};

/// The error type for Equihash
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
#[error("invalid equihash solution for BlockHeader")]
pub struct Error(#[from] equihash::Error);

/// The size of an Equihash solution in bytes (always 1344).
pub(crate) const SOLUTION_SIZE: usize = 1344;

/// Equihash Solution.
///
/// A wrapper around [u8; 1344] because Rust doesn't implement common
/// traits like `Debug`, `Clone`, etc for collections like array
/// beyond lengths 0 to 32.
///
/// The size of an Equihash solution in bytes is always 1344 so the
/// length of this type is fixed.
#[derive(Deserialize, Serialize)]
pub struct Solution(#[serde(with = "BigArray")] pub [u8; SOLUTION_SIZE]);

impl Solution {
    /// The length of the portion of the header used as input when verifying
    /// equihash solutions, in bytes.
    ///
    /// Excludes the 32-byte nonce, which is passed as a separate argument
    /// to the verification function.
    pub const INPUT_LENGTH: usize = 4 + 32 * 3 + 4 * 2;

    /// Returns `Ok(())` if `EquihashSolution` is valid for `header`
    pub fn check(&self, header: &Header) -> Result<(), Error> {
        let n = 200;
        let k = 9;
        let nonce = &header.nonce;
        let solution = &self.0;
        let mut input = Vec::new();

        header
            .zcash_serialize(&mut input)
            .expect("serialization into a vec can't fail");

        let input = &input[0..Solution::INPUT_LENGTH];

        equihash::is_valid_solution(n, k, input, nonce, solution)?;

        Ok(())
    }
}

impl PartialEq<Solution> for Solution {
    fn eq(&self, other: &Solution) -> bool {
        self.0.as_ref() == other.0.as_ref()
    }
}

impl fmt::Debug for Solution {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("EquihashSolution")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for Solution {}

impl Clone for Solution {
    fn clone(&self) -> Self {
        let mut bytes = [0; SOLUTION_SIZE];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
    }
}

impl Eq for Solution {}

impl ZcashSerialize for Solution {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.0.to_vec(), writer)
    }
}

impl ZcashDeserialize for Solution {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let solution: Vec<u8> = (&mut reader).zcash_deserialize_into()?;

        if solution.len() != SOLUTION_SIZE {
            return Err(SerializationError::Parse(
                "incorrect equihash solution size",
            ));
        }

        let mut bytes = [0; SOLUTION_SIZE];
        // Won't panic, because we just checked the length.
        bytes.copy_from_slice(&solution);

        Ok(Self(bytes))
    }
}
