//! Sapling nullifiers.

use crate::fmt::HexDebug;

/// A Nullifier for Sapling transactions
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Nullifier(pub HexDebug<[u8; 32]>);

impl From<[u8; 32]> for Nullifier {
    fn from(buf: [u8; 32]) -> Self {
        Self(buf.into())
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        *n.0
    }
}

impl From<Nullifier> for [jubjub::Fq; 2] {
    /// Add the nullifier through multiscalar packing
    ///
    /// Informed by <https://github.com/zkcrypto/bellman/blob/main/src/gadgets/multipack.rs>
    fn from(n: Nullifier) -> Self {
        use std::ops::AddAssign;

        let nullifier_bits_le: Vec<bool> =
            n.0.iter()
                .flat_map(|&v| (0..8).map(move |i| (v >> i) & 1 == 1))
                .collect();

        // The number of bits needed to represent the modulus, minus 1.
        const CAPACITY: usize = 255 - 1;

        let mut result = [jubjub::Fq::zero(); 2];

        // Since we know the max bits of the input (256) and the chunk size
        // (254), this will always result in 2 chunks.
        for (i, bits) in nullifier_bits_le.chunks(CAPACITY).enumerate() {
            let mut cur = jubjub::Fq::zero();
            let mut coeff = jubjub::Fq::one();

            for bit in bits {
                if *bit {
                    cur.add_assign(&coeff);
                }

                coeff = coeff.double();
            }

            result[i] = cur
        }

        result
    }
}
