//! Signature hashes for Zcash transactions

use super::Transaction;

use crate::parameters::NetworkUpgrade;
use crate::transparent;

use crate::primitives::zcash_primitives::{sighash, PrecomputedTxData};

bitflags::bitflags! {
    /// The different SigHash types, as defined in <https://zips.z.cash/zip-0143>
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct HashType: u32 {
        /// Sign all the outputs
        const ALL = 0b0000_0001;
        /// Sign none of the outputs - anyone can spend
        const NONE = 0b0000_0010;
        /// Sign one of the outputs - anyone can spend the rest
        const SINGLE = Self::ALL.bits() | Self::NONE.bits();
        /// Anyone can add inputs to this transaction
        const ANYONECANPAY = 0b1000_0000;
    }
}

/// A Signature Hash (or SIGHASH) as specified in
/// <https://zips.z.cash/protocol/protocol.pdf#sighash>
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct SigHash(pub [u8; 32]);

impl AsRef<[u8; 32]> for SigHash {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl AsRef<[u8]> for SigHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A SigHasher context which stores precomputed data that is reused
/// between sighash computations for the same transaction.
#[derive(Debug)]
pub struct SigHasher<'a> {
    precomputed_tx_data: PrecomputedTxData<'a>,
}

impl<'a> SigHasher<'a> {
    /// Create a new SigHasher for the given transaction.
    ///
    /// # Panics
    ///
    /// - If `trans` can't be converted to its `librustzcash` equivalent. This could happen, for
    ///   example, if `trans` contains the `nConsensusBranchId` field, and `nu` doesn't match it.
    ///   More details in [`PrecomputedTxData::new`].
    /// - If `nu` doesn't contain a consensus branch id convertible to its `librustzcash`
    ///   equivalent.
    pub fn new(
        trans: &'a Transaction,
        nu: NetworkUpgrade,
        all_previous_outputs: &'a [transparent::Output],
    ) -> Self {
        SigHasher {
            precomputed_tx_data: PrecomputedTxData::new(trans, nu, all_previous_outputs),
        }
    }

    /// Calculate the sighash for the current transaction.
    ///
    /// # Details
    ///
    /// The `input_index_script_code` tuple indicates the index of the
    /// transparent Input for which we are producing a sighash and the
    /// respective script code being validated, or None if it's a shielded
    /// input.
    ///
    /// # Panics
    ///
    /// - if the input index is out of bounds for self.inputs()
    pub fn sighash(
        &self,
        hash_type: HashType,
        input_index_script_code: Option<(usize, Vec<u8>)>,
    ) -> SigHash {
        sighash(
            &self.precomputed_tx_data,
            hash_type,
            input_index_script_code,
        )
    }
}
