//! Equihash Solution and related items.

use std::{fmt, io};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

use crate::{
    serde_helpers,
    serialization::{
        ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
    },
};

/// The size of an Equihash solution in bytes (always 1344).
pub(crate) const EQUIHASH_SOLUTION_SIZE: usize = 1344;

/// Equihash Solution.
///
/// A wrapper around [u8; 1344] because Rust doesn't implement common
/// traits like `Debug`, `Clone`, etc for collections like array
/// beyond lengths 0 to 32.
///
/// The size of an Equihash solution in bytes is always 1344 so the
/// length of this type is fixed.
#[derive(Deserialize, Serialize)]
pub struct EquihashSolution(
    #[serde(with = "serde_helpers::BigArray")] pub [u8; EQUIHASH_SOLUTION_SIZE],
);

impl EquihashSolution {
    /// The length of the portion of the header used as input when verifying
    /// equihash solutions, in bytes
    pub const INPUT_LENGTH: usize = 4 + 32 * 3 + 4 * 2;

    /// Validate an equihash solution
    pub(crate) fn is_valid(&self, input: &[u8], nonce: &[u8]) -> Result<(), equihash::Error> {
        assert_eq!(Self::INPUT_LENGTH, input.len());

        let n = 200;
        let k = 9;

        equihash::is_valid_solution(n, k, input, nonce, &self.0)
    }
}

impl PartialEq<EquihashSolution> for EquihashSolution {
    fn eq(&self, other: &EquihashSolution) -> bool {
        self.0.as_ref() == other.0.as_ref()
    }
}

impl fmt::Debug for EquihashSolution {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("EquihashSolution")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for EquihashSolution {}

impl Clone for EquihashSolution {
    fn clone(&self) -> Self {
        let mut bytes = [0; EQUIHASH_SOLUTION_SIZE];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
    }
}

impl Eq for EquihashSolution {}

impl ZcashSerialize for EquihashSolution {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(EQUIHASH_SOLUTION_SIZE as u64)?;
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for EquihashSolution {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let solution_size = reader.read_compactsize()?;
        if solution_size != (EQUIHASH_SOLUTION_SIZE as u64) {
            return Err(SerializationError::Parse(
                "incorrect equihash solution size",
            ));
        }
        let mut bytes = [0; EQUIHASH_SOLUTION_SIZE];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
impl Arbitrary for EquihashSolution {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), EQUIHASH_SOLUTION_SIZE))
            .prop_map(|v| {
                let mut bytes = [0; EQUIHASH_SOLUTION_SIZE];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, BlockHeader};

    proptest! {

        #[test]
        fn equihash_solution_roundtrip(solution in any::<EquihashSolution>()) {
            zebra_test::init();
            let mut data = Vec::new();

            solution.zcash_serialize(&mut data).expect("randomized EquihashSolution should serialize");

            let solution2 = EquihashSolution::zcash_deserialize(&data[..])
                .expect("randomized EquihashSolution should deserialize");

            prop_assert_eq![solution, solution2];
        }
    }

    const EQUIHASH_SOLUTION_BLOCK_OFFSET: usize = EquihashSolution::INPUT_LENGTH + 32;

    #[test]
    fn equihash_solution_test_vector() {
        zebra_test::init();
        let solution_bytes =
            &zebra_test::vectors::HEADER_MAINNET_415000_BYTES[EQUIHASH_SOLUTION_BLOCK_OFFSET..];
        let solution = EquihashSolution::zcash_deserialize(solution_bytes)
            .expect("Test vector EquihashSolution should deserialize");

        let mut data = Vec::new();
        solution
            .zcash_serialize(&mut data)
            .expect("Test vector EquihashSolution should serialize");

        assert_eq!(solution_bytes, data.as_slice());
    }

    #[test]
    fn equihash_solution_test_vector_is_valid() -> color_eyre::eyre::Result<()> {
        zebra_test::init();
        let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block test vector should deserialize");

        let solution = block.header.solution;
        let header_bytes =
            &zebra_test::vectors::HEADER_MAINNET_415000_BYTES[..EquihashSolution::INPUT_LENGTH];

        solution.is_valid(header_bytes, &block.header.nonce)?;

        Ok(())
    }

    prop_compose! {
        fn randomized_solutions(real_header: BlockHeader)
            (fake_solution in any::<EquihashSolution>()
                .prop_filter("solution must not be the actual solution", move |s| {
                    s != &real_header.solution
                })
            ) -> EquihashSolution {
            fake_solution
        }
    }

    #[test]
    fn equihash_prop_test_solution() -> color_eyre::eyre::Result<()> {
        zebra_test::init();

        for block_bytes in zebra_test::vectors::TEST_BLOCKS.iter() {
            let block = crate::block::Block::zcash_deserialize(&block_bytes[..])
                .expect("block test vector should deserialize");
            let solution = block.header.solution;
            let header_bytes = &block_bytes[..EquihashSolution::INPUT_LENGTH];

            solution.is_valid(header_bytes, &block.header.nonce)?;

            proptest!(|(fake_solution in randomized_solutions(block.header))| {
                fake_solution
                    .is_valid(header_bytes, &block.header.nonce)
                    .expect_err("block header should not validate on randomized solution");
            });
        }

        Ok(())
    }

    prop_compose! {
        fn randomized_nonce(real_nonce: [u8; 32])
            (fake_nonce in proptest::array::uniform32(any::<u8>())
                .prop_filter("nonce must not be the actual nonce", move |fake_nonce| {
                    fake_nonce != &real_nonce
                })
            ) -> [u8; 32] {
                fake_nonce
        }
    }

    #[test]
    fn equihash_prop_test_nonce() -> color_eyre::eyre::Result<()> {
        zebra_test::init();

        for block_bytes in zebra_test::vectors::TEST_BLOCKS.iter() {
            let block = crate::block::Block::zcash_deserialize(&block_bytes[..])
                .expect("block test vector should deserialize");
            let solution = block.header.solution;
            let header_bytes = &block_bytes[..EquihashSolution::INPUT_LENGTH];

            solution.is_valid(header_bytes, &block.header.nonce)?;

            proptest!(|(fake_nonce in randomized_nonce(block.header.nonce))| {
                solution
                    .is_valid(header_bytes, &fake_nonce)
                    .expect_err("equihash solution should not validate on randomized nonce");
            });
        }

        Ok(())
    }

    prop_compose! {
        fn randomized_input(real_input: Vec<u8>)
            (fake_input in vec(any::<u8>(), EquihashSolution::INPUT_LENGTH)
                .prop_filter("input must not be the actual input", move |fake_input| {
                    fake_input != &real_input
                })
            ) -> Vec<u8> {
                fake_input
        }
    }

    #[test]
    fn equihash_prop_test_input() -> color_eyre::eyre::Result<()> {
        zebra_test::init();

        for block_bytes in zebra_test::vectors::TEST_BLOCKS.iter() {
            let block = crate::block::Block::zcash_deserialize(&block_bytes[..])
                .expect("block test vector should deserialize");
            let solution = block.header.solution;
            let header_bytes = &block_bytes[..EquihashSolution::INPUT_LENGTH];

            solution.is_valid(header_bytes, &block.header.nonce)?;

            proptest!(|(fake_input in randomized_input(header_bytes.into()))| {
                solution
                    .is_valid(fake_input.as_slice(), &block.header.nonce)
                    .expect_err("equihash solution should not validate on randomized input");
            });
        }

        Ok(())
    }

    static EQUIHASH_SIZE_TESTS: &[u64] = &[
        0,
        1,
        (EQUIHASH_SOLUTION_SIZE - 1) as u64,
        EQUIHASH_SOLUTION_SIZE as u64,
        (EQUIHASH_SOLUTION_SIZE + 1) as u64,
        u64::MAX - 1,
        u64::MAX,
    ];

    #[test]
    fn equihash_solution_size_field() {
        zebra_test::init();

        for size in EQUIHASH_SIZE_TESTS {
            let mut data = Vec::new();
            data.write_compactsize(*size as u64)
                .expect("Compact size should serialize");
            data.resize(data.len() + EQUIHASH_SOLUTION_SIZE, 0);
            let result = EquihashSolution::zcash_deserialize(data.as_slice());
            if *size == (EQUIHASH_SOLUTION_SIZE as u64) {
                result.expect("Correct size field in EquihashSolution should deserialize");
            } else {
                result
                    .expect_err("Wrong size field in EquihashSolution should fail on deserialize");
            }
        }
    }
}
