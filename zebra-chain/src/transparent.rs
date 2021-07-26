//! Transparent-related (Bitcoin-inherited) functionality.
#![allow(clippy::unit_arg)]

mod address;
mod keys;
mod script;
mod serialize;
mod utxo;

pub use address::Address;
pub use script::Script;
pub use utxo::{
    new_ordered_outputs, new_outputs, utxos_from_ordered_utxos, CoinbaseSpendRestriction,
    OrderedUtxo, Utxo, MIN_TRANSPARENT_COINBASE_MATURITY,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod prop;

use crate::{
    amount::{Amount, NonNegative},
    block, transaction,
};

/// Arbitrary data inserted by miners into a coinbase transaction.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoinbaseData(
    /// Invariant: this vec, together with the coinbase height, must be less than
    /// 100 bytes. We enforce this by only constructing CoinbaseData fields by
    /// parsing blocks with 100-byte data fields. When we implement block
    /// creation, we should provide a constructor for the coinbase data field
    /// that restricts it to 95 = 100 -1 -4 bytes (safe for any block height up
    /// to 500_000_000).
    pub(super) Vec<u8>,
);

#[cfg(any(test, feature = "proptest-impl"))]
impl CoinbaseData {
    /// Create a new `CoinbaseData` containing `data`.
    ///
    /// Only for use in tests.
    pub fn new(data: Vec<u8>) -> CoinbaseData {
        CoinbaseData(data)
    }
}

impl AsRef<[u8]> for CoinbaseData {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for CoinbaseData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let escaped = String::from_utf8(
            self.0
                .iter()
                .cloned()
                .flat_map(std::ascii::escape_default)
                .collect(),
        )
        .expect("ascii::escape_default produces utf8");
        f.debug_tuple("CoinbaseData").field(&escaped).finish()
    }
}

/// OutPoint
///
/// A particular transaction output reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct OutPoint {
    /// References the transaction that contains the UTXO being spent.
    pub hash: transaction::Hash,

    /// Identifies which UTXO from that transaction is referenced; the
    /// first output is 0, etc.
    pub index: u32,
}

/// A transparent input to a transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Input {
    /// A reference to an output of a previous transaction.
    PrevOut {
        /// The previous output transaction reference.
        outpoint: OutPoint,
        /// The script that authorizes spending `outpoint`.
        unlock_script: Script,
        /// The sequence number for the output.
        sequence: u32,
    },
    /// New coins created by the block reward.
    Coinbase {
        /// The height of this block.
        height: block::Height,
        /// Free data inserted by miners after the block height.
        data: CoinbaseData,
        /// The sequence number for the output.
        sequence: u32,
    },
}

impl Input {
    /// If this is a `PrevOut` input, returns this input's outpoint.
    /// Otherwise, returns `None`.
    pub fn outpoint(&self) -> Option<OutPoint> {
        if let Input::PrevOut { outpoint, .. } = self {
            Some(*outpoint)
        } else {
            None
        }
    }

    /// Set this input's outpoint.
    ///
    /// Should only be called on `PrevOut` inputs.
    ///
    /// # Panics
    ///
    /// If `self` is a coinbase input.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn set_outpoint(&mut self, new_outpoint: OutPoint) {
        if let Input::PrevOut {
            ref mut outpoint, ..
        } = self
        {
            *outpoint = new_outpoint;
        } else {
            unreachable!("unexpected variant: Coinbase Inputs do not have OutPoints");
        }
    }
}

/// A transparent output from a transaction.
///
/// The most fundamental building block of a transaction is a
/// transaction output -- the ZEC you own in your "wallet" is in
/// fact a subset of unspent transaction outputs (or "UTXO"s) of the
/// global UTXO set.
///
/// UTXOs are indivisible, discrete units of value which can only be
/// consumed in their entirety. Thus, if I want to send you 1 ZEC and
/// I only own one UTXO worth 2 ZEC, I would construct a transaction
/// that spends my UTXO and sends 1 ZEC to you and 1 ZEC back to me
/// (just like receiving change).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Output {
    /// Transaction value.
    // At https://en.bitcoin.it/wiki/Protocol_documentation#tx, this is an i64.
    pub value: Amount<NonNegative>,

    /// The lock script defines the conditions under which this output can be spent.
    pub lock_script: Script,
}
