//! Signature hashes for Zcash transactions

use super::Transaction;

use crate::{parameters::NetworkUpgrade, transparent};

use crate::primitives::zcash_primitives::sighash;

static ZIP143_EXPLANATION: &str = "Invalid transaction version: after Overwinter activation transaction versions 1 and 2 are rejected";

bitflags::bitflags! {
    /// The different SigHash types, as defined in https://zips.z.cash/zip-0143
    pub struct HashType: u32 {
        /// Sign all the outputs
        const ALL = 0b0000_0001;
        /// Sign none of the outputs - anyone can spend
        const NONE = 0b0000_0010;
        /// Sign one of the outputs - anyone can spend the rest
        const SINGLE = Self::ALL.bits | Self::NONE.bits;
        /// Anyone can add inputs to this transaction
        const ANYONECANPAY = 0b1000_0000;
    }
}

/// A Signature Hash (or SIGHASH) as specified in
/// https://zips.z.cash/protocol/protocol.pdf#sighash
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
    network_upgrade: NetworkUpgrade,
    input: Option<(transparent::Output, &'a transparent::Input, usize)>,
}

impl<'a> SigHasher<'a> {
    pub fn new(
        trans: &'a Transaction,
        hash_type: HashType,
        network_upgrade: NetworkUpgrade,
        input: Option<(u32, transparent::Output)>,
    ) -> Self {
        let input = if let Some((index, prevout)) = input {
            let index = index as usize;
            let inputs = trans.inputs();

            Some((prevout, &inputs[index], index))
        } else {
            None
        };

        SigHasher {
            trans,
            hash_type,
            network_upgrade,
            input,
        }
    }

    pub(super) fn sighash(self) -> SigHash {
        use NetworkUpgrade::*;

        match self.network_upgrade {
            Genesis | BeforeOverwinter => unreachable!(ZIP143_EXPLANATION),
            Overwinter | Sapling | Blossom | Heartwood | Canopy | Nu5 => {
                self.hash_sighash_librustzcash()
            }
        }
    }

    /// Compute a signature hash using librustzcash.
    fn hash_sighash_librustzcash(&self) -> SigHash {
        let input = self
            .input
            .as_ref()
            .map(|(output, input, idx)| (output, *input, *idx));
        sighash(self.trans, self.hash_type, self.network_upgrade, input)
    }
}
