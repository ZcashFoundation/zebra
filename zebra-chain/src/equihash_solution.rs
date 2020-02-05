//! Equihash Solution and related items.

use std::{fmt, io};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};

/// The size of an Equihash solution in bytes (always 1344).
const EQUIHASH_SOLUTION_SIZE: usize = 1344;

/// Equihash Solution.
///
/// A wrapper around [u8; 1344] because Rust doesn't implement common
/// traits like `Debug`, `Clone`, etc for collections like array
/// beyond lengths 0 to 32.
///
/// The size of an Equihash solution in bytes is always 1344 so the
/// length of this type is fixed.
pub struct EquihashSolution(pub [u8; EQUIHASH_SOLUTION_SIZE]);

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
        reader.read_compactsize()?;
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
                return Self(bytes);
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use super::*;

    proptest! {

        #[test]
        fn equihash_solution_roundtrip(solution in any::<EquihashSolution>()) {

            let mut data = Vec::new();

            solution.zcash_serialize(&mut data).expect("EquihashSolution should serialize");

            let solution2 = EquihashSolution::zcash_deserialize(&data[..])
                .expect("randomized EquihashSolution should deserialize");

            prop_assert_eq![solution, solution2];
        }

    }
}
