//! Equihash Solution and related items.

// use std::fmt;

// use hex::ToHex;

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
#[derive(Clone)]
pub struct EquihashSolution([u8; EQUIHASH_SOLUTION_SIZE]);

impl Default for EquihashSolution {
    fn default() -> Self {
        EquihashSolution([0; EQUIHASH_SOLUTION_SIZE])
    }
}

// TODO: fix hex crate import conflict with tracing-subscriber
// impl fmt::Debug for EquihashSolution {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         f.write_str(&self.0.encode_hex::<String>())
//     }
// }

impl PartialEq<EquihashSolution> for EquihashSolution {
    fn eq(&self, other: &EquihashSolution) -> bool {
        self.0.as_ref() == other.0.as_ref()
    }
}
