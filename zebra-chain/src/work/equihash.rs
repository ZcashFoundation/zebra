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

#[cfg(feature = "internal-miner")]
use crate::serialization::AtLeastOne;

/// The error type for Equihash validation.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
#[error("invalid equihash solution for BlockHeader")]
pub struct Error(#[from] equihash::Error);

/// The error type for Equihash solving.
#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("solver was cancelled")]
pub struct SolverCancelled;

/// The size of an Equihash solution in bytes (always 1344).
pub(crate) const SOLUTION_SIZE: usize = 1344;

/// Equihash Solution in compressed format.
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
    #[allow(clippy::unwrap_in_result)]
    pub fn check(&self, header: &Header) -> Result<(), Error> {
        let n = 200;
        let k = 9;
        let nonce = &header.nonce;
        let solution = &self.0;
        let mut input = Vec::new();

        header
            .zcash_serialize(&mut input)
            .expect("serialization into a vec can't fail");

        // The part of the header before the nonce and solution.
        // This data is kept constant during solver runs, so the verifier API takes it separately.
        let input = &input[0..Solution::INPUT_LENGTH];

        equihash::is_valid_solution(n, k, input, nonce.as_ref(), solution)?;

        Ok(())
    }

    /// Returns a [`Solution`] containing the bytes from `solution`.
    /// Returns an error if `solution` is the wrong length.
    pub fn from_bytes(solution: &[u8]) -> Result<Self, SerializationError> {
        if solution.len() != SOLUTION_SIZE {
            return Err(SerializationError::Parse(
                "incorrect equihash solution size",
            ));
        }

        let mut bytes = [0; SOLUTION_SIZE];
        // Won't panic, because we just checked the length.
        bytes.copy_from_slice(solution);

        Ok(Self(bytes))
    }

    /// Returns a [`Solution`] of `[0; SOLUTION_SIZE]` to be used in block proposals.
    #[cfg(feature = "getblocktemplate-rpcs")]
    pub fn for_proposal() -> Self {
        Self([0; SOLUTION_SIZE])
    }

    /// Mines and returns one or more [`Solution`]s based on a template `header`.
    /// The returned header contains a valid `nonce` and `solution`.
    ///
    /// If `cancel_fn()` returns an error, returns early with `Err(SolverCancelled)`.
    ///
    /// The `nonce` in the header template is taken as the starting nonce. If you are running multiple
    /// solvers at the same time, start them with different nonces.
    /// The `solution` in the header template is ignored.
    ///
    /// This method is CPU and memory-intensive. It uses 144 MB of RAM and one CPU core while running.
    /// It can run for minutes or hours if the network difficulty is high.
    #[cfg(feature = "internal-miner")]
    #[allow(clippy::unwrap_in_result)]
    pub fn solve<F>(
        mut header: Header,
        mut cancel_fn: F,
    ) -> Result<AtLeastOne<Header>, SolverCancelled>
    where
        F: FnMut() -> Result<(), SolverCancelled>,
    {
        use crate::shutdown::is_shutting_down;

        let mut input = Vec::new();
        header
            .zcash_serialize(&mut input)
            .expect("serialization into a vec can't fail");
        // Take the part of the header before the nonce and solution.
        // This data is kept constant for this solver run.
        let input = &input[0..Solution::INPUT_LENGTH];

        while !is_shutting_down() {
            // Don't run the solver if we'd just cancel it anyway.
            cancel_fn()?;

            let solutions = equihash_solver::tromp::solve_200_9_compressed(input, || {
                // Cancel the solver if we have a new template.
                if cancel_fn().is_err() {
                    return None;
                }

                // This skips the first nonce, which doesn't matter in practice.
                Self::next_nonce(&mut header.nonce);
                Some(*header.nonce)
            });

            let mut valid_solutions = Vec::new();

            // If we got any solutions, try submitting them, because the new template might just
            // contain some extra transactions. Mining extra transactions is optional.
            for solution in &solutions {
                header.solution = Self::from_bytes(solution)
                    .expect("unexpected invalid solution: incorrect length");

                // TODO: work out why we sometimes get invalid solutions here
                if let Err(error) = header.solution.check(&header) {
                    info!(?error, "found invalid solution for header");
                    continue;
                }

                if Self::difficulty_is_valid(&header) {
                    valid_solutions.push(header);
                }
            }

            match valid_solutions.try_into() {
                Ok(at_least_one_solution) => return Ok(at_least_one_solution),
                Err(_is_empty_error) => debug!(
                    solutions = ?solutions.len(),
                    "found valid solutions which did not pass the validity or difficulty checks"
                ),
            }
        }

        Err(SolverCancelled)
    }

    /// Modifies `nonce` to be the next integer in big-endian order.
    /// Wraps to zero if the next nonce would overflow.
    #[cfg(feature = "internal-miner")]
    fn next_nonce(nonce: &mut [u8; 32]) {
        let _ignore_overflow = crate::primitives::byte_array::increment_big_endian(&mut nonce[..]);
    }

    /// Returns `true` if the `nonce` and `solution` in `header` meet the difficulty threshold.
    ///
    /// Assumes that the difficulty threshold in the header is valid.
    #[cfg(feature = "internal-miner")]
    fn difficulty_is_valid(header: &Header) -> bool {
        // Simplified from zebra_consensus::block::check::difficulty_is_valid().
        let difficulty_threshold = header
            .difficulty_threshold
            .to_expanded()
            .expect("unexpected invalid header template: invalid difficulty threshold");

        // TODO: avoid calculating this hash multiple times
        let hash = header.hash();

        // Note: this comparison is a u256 integer comparison, like zcashd and bitcoin. Greater
        // values represent *less* work.
        hash <= difficulty_threshold
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
        *self
    }
}

impl Eq for Solution {}

#[cfg(any(test, feature = "proptest-impl"))]
impl Default for Solution {
    fn default() -> Self {
        Self([0; SOLUTION_SIZE])
    }
}

impl ZcashSerialize for Solution {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.0.to_vec(), writer)
    }
}

impl ZcashDeserialize for Solution {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let solution: Vec<u8> = (&mut reader).zcash_deserialize_into()?;
        Self::from_bytes(&solution)
    }
}
