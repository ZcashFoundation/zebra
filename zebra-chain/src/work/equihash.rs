//! Equihash Solution and related items.

use std::{fmt, io};

use hex::{FromHex, FromHexError, ToHex};
use serde_big_array::BigArray;

use crate::{
    block::{Header, ZCASH_BLOCK_VERSION},
    parameters::Network,
    serialization::{
        zcash_deserialize_bytes_external_count, zcash_serialize_bytes, CompactSizeMessage,
        SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
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

/// The size of an Equihash solution in bytes on Regtest (always 36).
pub(crate) const REGTEST_SOLUTION_SIZE: usize = 36;

/// Equihash Solution in compressed format.
///
/// A wrapper around `[u8; n]` where `n` is the solution size because
/// Rust doesn't implement common traits like `Debug`, `Clone`, etc.
/// for collections like arrays beyond lengths 0 to 32.
///
/// The size of an Equihash solution in bytes is always 1344 on Mainnet and Testnet, and
/// is always 36 on Regtest so the length of this type is fixed.
#[derive(Deserialize, Serialize)]
// It's okay to use the extra space on Regtest
#[allow(clippy::large_enum_variant)]
pub enum Solution {
    /// Equihash solution on Mainnet or Testnet
    Common(#[serde(with = "BigArray")] [u8; SOLUTION_SIZE]),
    /// Equihash solution on Regtest
    Regtest(#[serde(with = "BigArray")] [u8; REGTEST_SOLUTION_SIZE]),
}

impl Solution {
    /// The length of the portion of the header used as input when verifying
    /// equihash solutions, in bytes.
    ///
    /// Excludes the 32-byte nonce, which is passed as a separate argument
    /// to the verification function.
    pub const INPUT_LENGTH: usize = 4 + 32 * 3 + 4 * 2;

    /// Returns the inner value of the [`Solution`] as a byte slice.
    fn value(&self) -> &[u8] {
        match self {
            Solution::Common(solution) => solution.as_slice(),
            Solution::Regtest(solution) => solution.as_slice(),
        }
    }

    /// Serializes just the header fields committed to by Equihash, excluding nonce and solution.
    ///
    /// Keep the field order and encodings identical to `Header::zcash_serialize`.
    pub(super) fn input(header: &Header) -> [u8; Self::INPUT_LENGTH] {
        // Preserve the serializer's invariants for headers constructed in memory.
        assert!(
            (ZCASH_BLOCK_VERSION..0x8000_0000).contains(&header.version),
            "deserialized and generated block versions are at least 4 with the high bit unset"
        );
        let time: u32 = header
            .time
            .timestamp()
            .try_into()
            .expect("deserialized and generated timestamps are u32 values");

        let mut input = [0; Self::INPUT_LENGTH];
        input[..4].copy_from_slice(&header.version.to_le_bytes());
        input[4..36].copy_from_slice(&header.previous_block_hash.0);
        input[36..68].copy_from_slice(&header.merkle_root.0);
        input[68..100].copy_from_slice(header.commitment_bytes.as_ref());
        input[100..104].copy_from_slice(&time.to_le_bytes());
        input[104..].copy_from_slice(&header.difficulty_threshold.0.to_le_bytes());
        input
    }

    /// Verifies the production Equihash (200, 9) proof for `header`.
    ///
    /// Regtest's short proofs are not accepted by this network-independent verifier.
    #[allow(clippy::unwrap_in_result)]
    pub fn check(&self, header: &Header) -> Result<(), Error> {
        self.check_with_input(header, &Self::input(header))
    }

    /// Checks a solution using a prefix already serialized for this header.
    fn check_with_input(
        &self,
        header: &Header,
        input: &[u8; Self::INPUT_LENGTH],
    ) -> Result<(), Error> {
        equihash::is_valid_solution(200, 9, input, header.nonce.as_ref(), self.value())?;

        Ok(())
    }

    /// Returns a [`Solution`] containing the bytes from `solution`.
    /// Returns an error if `solution` is the wrong length.
    pub fn from_bytes(solution: &[u8]) -> Result<Self, SerializationError> {
        match solution.len() {
            // Won't panic, because we just checked the length.
            SOLUTION_SIZE => {
                let mut bytes = [0; SOLUTION_SIZE];
                bytes.copy_from_slice(solution);
                Ok(Self::Common(bytes))
            }
            REGTEST_SOLUTION_SIZE => {
                let mut bytes = [0; REGTEST_SOLUTION_SIZE];
                bytes.copy_from_slice(solution);
                Ok(Self::Regtest(bytes))
            }
            _unexpected_len => Err(SerializationError::Parse(
                "incorrect equihash solution size",
            )),
        }
    }

    /// The serialized size of a solution on Mainnet and Testnet (except Regtest), in bytes:
    /// the 1344-byte solution and its 3-byte CompactSize length prefix (`0xfd` + `u16`).
    pub const SERIALIZED_SIZE: usize = 3 + SOLUTION_SIZE;

    /// The serialized size of a solution on Regtest, in bytes:
    /// the 36-byte solution and its 1-byte CompactSize length prefix.
    pub const REGTEST_SERIALIZED_SIZE: usize = 1 + REGTEST_SOLUTION_SIZE;

    /// Returns the size of the serialized solution on `network`, in bytes,
    /// including its CompactSize length prefix.
    ///
    /// The solution size is constant per network, so this is also constant per network:
    /// [`Self::REGTEST_SERIALIZED_SIZE`] on Regtest, [`Self::SERIALIZED_SIZE`] everywhere else.
    pub fn serialized_size(network: &Network) -> usize {
        if network.is_regtest() {
            Self::REGTEST_SERIALIZED_SIZE
        } else {
            Self::SERIALIZED_SIZE
        }
    }

    /// Returns a [`Solution`] of `[0; SOLUTION_SIZE]` to be used in block proposals.
    pub fn for_proposal() -> Self {
        // TODO: Accept network as an argument, and if it's Regtest, return the shorter null solution.
        Self::Common([0; SOLUTION_SIZE])
    }

    /// Mines and returns one or more [`Solution`]s based on a template `header`.
    /// The returned header contains a valid `nonce` and `solution`.
    ///
    /// If `cancel_fn()` returns an error, returns early with `Err(SolverCancelled)`.
    /// Cancellation and shutdown are checked before solving and between nonce attempts, not between
    /// digit rounds. An in-progress nonce attempt must finish before its solutions are discarded.
    /// Cancellation is checked again before returning solved headers.
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

        // This prefix stays constant for the entire solver run.
        let input = Self::input(&header);

        while !is_shutting_down() {
            // Don't run the solver if we'd just cancel it anyway.
            cancel_fn()?;

            let mut cancelled = false;
            let solutions = equihash::tromp::solve_200_9(&input, || {
                if is_shutting_down() || cancel_fn().is_err() {
                    cancelled = true;
                    return None;
                }

                // This skips the first nonce, which doesn't matter in practice.
                Self::next_nonce(&mut header.nonce);
                Some(*header.nonce)
            });

            // The stock solver returns an empty vector when its nonce callback cancels. Remember
            // that cancellation even if cancel_fn would subsequently return Ok.
            if cancelled || is_shutting_down() {
                return Err(SolverCancelled);
            }
            // A successful nonce attempt can finish without requesting another nonce.
            cancel_fn()?;

            let mut valid_solutions = Vec::new();

            for solution in &solutions {
                header.solution = Self::from_bytes(solution)
                    .expect("unexpected invalid solution: incorrect length");

                // TODO: work out why we sometimes get invalid solutions here
                if let Err(error) = header.solution.check_with_input(&header, &input) {
                    info!(?error, "found invalid solution for header");
                    continue;
                }

                if Self::difficulty_is_valid(&header) {
                    valid_solutions.push(header);
                }
            }

            match valid_solutions.try_into() {
                Ok(at_least_one_solution) => {
                    if is_shutting_down() {
                        return Err(SolverCancelled);
                    }
                    cancel_fn()?;
                    return Ok(at_least_one_solution);
                }
                Err(_is_empty_error) => debug!(
                    solutions = ?solutions.len(),
                    "found valid solutions which did not pass the validity or difficulty checks"
                ),
            }
        }

        Err(SolverCancelled)
    }

    /// Returns `true` if the `nonce` and `solution` in `header` meet the difficulty threshold.
    ///
    /// # Panics
    ///
    /// - If `header` contains an invalid difficulty threshold.
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

    /// Modifies `nonce` to be the next integer in big-endian order.
    /// Wraps to zero if the next nonce would overflow.
    #[cfg(feature = "internal-miner")]
    fn next_nonce(nonce: &mut [u8; 32]) {
        let _ignore_overflow = crate::primitives::byte_array::increment_big_endian(&mut nonce[..]);
    }
}

impl PartialEq<Solution> for Solution {
    fn eq(&self, other: &Solution) -> bool {
        self.value() == other.value()
    }
}

impl fmt::Debug for Solution {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("EquihashSolution")
            .field(&hex::encode(self.value()))
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
        Self::Common([0; SOLUTION_SIZE])
    }
}

impl ZcashSerialize for Solution {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        zcash_serialize_bytes(&self.value().to_vec(), writer)
    }

    fn zcash_serialized_size(&self) -> usize {
        let len = self.value().len();
        CompactSizeMessage::try_from(len)
            .expect("solution length fits in MAX_PROTOCOL_MESSAGE_LEN")
            .zcash_serialized_size()
            + len
    }
}

impl ZcashDeserialize for Solution {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len: CompactSizeMessage = (&mut reader).zcash_deserialize_into()?;
        let len: usize = len.into();

        // Validate the length against the consensus-required sizes before
        // allocating, so an attacker-controlled CompactSize cannot force a
        // multi-megabyte allocation.
        if len > SOLUTION_SIZE {
            return Err(SerializationError::Parse(
                "incorrect equihash solution size",
            ));
        }

        let solution = zcash_deserialize_bytes_external_count(len, &mut reader)?;
        Self::from_bytes(&solution)
    }
}

impl ToHex for &Solution {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.value().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.value().encode_hex_upper()
    }
}

impl ToHex for Solution {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for Solution {
    type Error = FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let bytes = Vec::from_hex(hex)?;
        Solution::from_bytes(&bytes).map_err(|_| FromHexError::InvalidStringLength)
    }
}
