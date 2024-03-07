//! Signature hashes for Zcash transactions

use super::Transaction;

use crate::parameters::ConsensusBranchId;
use crate::transparent;

use crate::primitives::zcash_primitives::sighash;

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

pub(super) struct SigHasher<'a> {
    trans: &'a Transaction,
    hash_type: HashType,
    branch_id: ConsensusBranchId,
    all_previous_outputs: &'a [transparent::Output],
    input_index: Option<usize>,
    script_code: Option<Vec<u8>>,
}

impl<'a> SigHasher<'a> {
    pub fn new(
        trans: &'a Transaction,
        hash_type: HashType,
        branch_id: ConsensusBranchId,
        all_previous_outputs: &'a [transparent::Output],
        input_index: Option<usize>,
        script_code: Option<Vec<u8>>,
    ) -> Self {
        SigHasher {
            trans,
            hash_type,
            branch_id,
            all_previous_outputs,
            input_index,
            script_code,
        }
    }

    pub(super) fn sighash(self) -> SigHash {
        self.hash_sighash_librustzcash()
    }

    /// Compute a signature hash using librustzcash.
    fn hash_sighash_librustzcash(&self) -> SigHash {
        sighash(
            self.trans,
            self.hash_type,
            self.branch_id,
            self.all_previous_outputs,
            self.input_index,
            self.script_code.clone(),
        )
    }
}
