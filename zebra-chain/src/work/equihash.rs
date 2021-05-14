//! Equihash Solution and related items.

use crate::block::Header;
use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize,
    ZcashSerialize,
};
use std::{fmt, io};

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
pub struct Solution(#[serde(with = "serde_helpers::BigArray")] pub [u8; SOLUTION_SIZE]);

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
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(SOLUTION_SIZE as u64)?;
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Solution {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let solution_size = reader.read_compactsize()?;
        if solution_size != (SOLUTION_SIZE as u64) {
            return Err(SerializationError::Parse(
                "incorrect equihash solution size",
            ));
        }
        let mut bytes = [0; SOLUTION_SIZE];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MAX_BLOCK_BYTES;

    static EQUIHASH_SIZE_TESTS: &[u64] = &[
        0,
        1,
        (SOLUTION_SIZE - 1) as u64,
        SOLUTION_SIZE as u64,
        (SOLUTION_SIZE + 1) as u64,
        MAX_BLOCK_BYTES - 1,
        MAX_BLOCK_BYTES,
    ];

    #[test]
    fn equihash_solution_size_field() {
        zebra_test::init();

        for size in EQUIHASH_SIZE_TESTS {
            let mut data = Vec::new();
            data.write_compactsize(*size as u64)
                .expect("Compact size should serialize");
            data.resize(data.len() + SOLUTION_SIZE, 0);
            let result = Solution::zcash_deserialize(data.as_slice());
            if *size == (SOLUTION_SIZE as u64) {
                result.expect("Correct size field in EquihashSolution should deserialize");
            } else {
                result
                    .expect_err("Wrong size field in EquihashSolution should fail on deserialize");
            }
        }
    }
}
