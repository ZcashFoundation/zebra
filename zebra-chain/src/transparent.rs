//! Transparent-related (Bitcoin-inherited) functionality.

mod address;
mod keys;
mod opcodes;
mod script;
mod serialize;
mod utxo;

use std::{collections::HashMap, fmt, iter, ops::AddAssign};

use zcash_script::{opcode::Evaluable as _, pattern::push_num};
use zcash_transparent::{address::TransparentAddress, bundle::TxOut};

use crate::{
    amount::{Amount, NonNegative},
    block,
    parameters::Network,
    serialization::ZcashSerialize,
    transaction,
    transparent::serialize::GENESIS_COINBASE_SCRIPT_SIG,
};

pub use address::Address;
pub use script::Script;
pub use utxo::{
    new_ordered_outputs, new_outputs, outputs_from_utxos, utxos_from_ordered_utxos,
    CoinbaseSpendRestriction, OrderedUtxo, Utxo,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub use utxo::{
    new_ordered_outputs_with_height, new_outputs_with_height, new_transaction_ordered_outputs,
};

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// The maturity threshold for transparent coinbase outputs.
///
/// "A transaction MUST NOT spend a transparent output of a coinbase transaction
/// from a block less than 100 blocks prior to the spend. Note that transparent
/// outputs of coinbase transactions include Founders' Reward outputs and
/// transparent Funding Stream outputs."
/// [7.1](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)
//
// TODO: change type to HeightDiff
pub const MIN_TRANSPARENT_COINBASE_MATURITY: u32 = 100;

/// The rate used to calculate the dust threshold, in zatoshis per 1000 bytes.
///
/// History: <https://github.com/zcash/zcash/blob/v6.10.0/src/policy/policy.h#L43-L89>
pub const ONE_THIRD_DUST_THRESHOLD_RATE: u32 = 100;

/// OutPoint
///
/// A particular transaction output reference.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[cfg_attr(
    any(test, feature = "proptest-impl", feature = "elasticsearch"),
    derive(Serialize)
)]
pub struct OutPoint {
    /// References the transaction that contains the UTXO being spent.
    ///
    /// # Correctness
    ///
    /// Consensus-critical serialization uses [`ZcashSerialize`].
    /// [`serde`]-based hex serialization must only be used for testing.
    #[cfg_attr(any(test, feature = "proptest-impl"), serde(with = "hex"))]
    pub hash: transaction::Hash,

    /// Identifies which UTXO from that transaction is referenced; the
    /// first output is 0, etc.
    // TODO: Use OutputIndex here
    pub index: u32,
}

impl OutPoint {
    /// Returns a new [`OutPoint`] from an in-memory output `index`.
    ///
    /// # Panics
    ///
    /// If `index` doesn't fit in a [`u32`].
    pub fn from_usize(hash: transaction::Hash, index: usize) -> OutPoint {
        OutPoint {
            hash,
            index: index
                .try_into()
                .expect("valid in-memory output indexes fit in a u32"),
        }
    }
}

/// A transparent input to a transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl", feature = "elasticsearch"),
    derive(Serialize)
)]
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
        /// Optional, arbitrary data miners can insert into a coinbase tx.
        /// Limited to ~ 94 bytes.
        data: Vec<u8>,
        /// The sequence number for the output.
        sequence: u32,
    },
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Input::PrevOut {
                outpoint,
                unlock_script,
                ..
            } => {
                let mut fmter = f.debug_struct("transparent::Input::PrevOut");

                fmter.field("unlock_script_len", &unlock_script.as_raw_bytes().len());
                fmter.field("outpoint", outpoint);

                fmter.finish()
            }
            Input::Coinbase { height, data, .. } => {
                let mut fmter = f.debug_struct("transparent::Input::Coinbase");

                fmter.field("height", height);
                fmter.field("data_len", &data.len());

                fmter.finish()
            }
        }
    }
}

impl Input {
    /// Returns the miner data in this input, if it is an [`Input::Coinbase`].
    pub fn miner_data(&self) -> Option<&Vec<u8>> {
        match self {
            Input::Coinbase { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Returns the full coinbase script (the encoded height along with the optional miner data) if
    /// this is an [`Input::Coinbase`]. Also returns `None` if the coinbase is for the genesis block
    /// but does not match the expected genesis coinbase data.
    pub fn coinbase_script(&self) -> Option<Vec<u8>> {
        match self {
            Input::PrevOut { .. } => None,
            Input::Coinbase { height, data, .. } => {
                if height.is_min() {
                    (data.as_slice() == GENESIS_COINBASE_SCRIPT_SIG)
                        .then_some(GENESIS_COINBASE_SCRIPT_SIG.to_vec())
                } else {
                    let mut script = push_num(height.into()).to_bytes();
                    script.extend_from_slice(data);

                    Some(script)
                }
            }
        }
    }

    /// Returns the input's sequence number.
    pub fn sequence(&self) -> u32 {
        match self {
            Input::PrevOut { sequence, .. } | Input::Coinbase { sequence, .. } => *sequence,
        }
    }

    /// Sets the input's sequence number.
    ///
    /// Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn set_sequence(&mut self, new_sequence: u32) {
        match self {
            Input::PrevOut { sequence, .. } | Input::Coinbase { sequence, .. } => {
                *sequence = new_sequence
            }
        }
    }

    /// If this is a [`Input::PrevOut`] input, returns this input's
    /// [`OutPoint`]. Otherwise, returns `None`.
    pub fn outpoint(&self) -> Option<OutPoint> {
        if let Input::PrevOut { outpoint, .. } = self {
            Some(*outpoint)
        } else {
            None
        }
    }

    /// Set this input's [`OutPoint`].
    ///
    /// Should only be called on [`Input::PrevOut`] inputs.
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

    /// Get the value spent by this input, by looking up its [`OutPoint`] in `outputs`.
    /// See [`Self::value`] for details.
    ///
    /// # Panics
    ///
    /// If the provided [`Output`]s don't have this input's [`OutPoint`].
    pub(crate) fn value_from_outputs(
        &self,
        outputs: &HashMap<OutPoint, Output>,
    ) -> Amount<NonNegative> {
        match self {
            Input::PrevOut { outpoint, .. } => {
                outputs
                    .get(outpoint)
                    .unwrap_or_else(|| {
                        panic!(
                            "provided Outputs (length {:?}) don't have spent {:?}",
                            outputs.len(),
                            outpoint
                        )
                    })
                    .value
            }
            Input::Coinbase { .. } => Amount::zero(),
        }
    }

    /// Get the value spent by this input, by looking up its [`OutPoint`] in
    /// [`Utxo`]s.
    ///
    /// This amount is added to the transaction value pool by this input.
    ///
    /// # Panics
    ///
    /// If the provided [`Utxo`]s don't have this input's [`OutPoint`].
    pub fn value(&self, utxos: &HashMap<OutPoint, utxo::Utxo>) -> Amount<NonNegative> {
        if let Some(outpoint) = self.outpoint() {
            // look up the specific Output and convert it to the expected format
            let output = utxos
                .get(&outpoint)
                .expect("provided Utxos don't have spent OutPoint")
                .output
                .clone();
            self.value_from_outputs(&iter::once((outpoint, output)).collect())
        } else {
            // coinbase inputs don't need any UTXOs
            self.value_from_outputs(&HashMap::new())
        }
    }

    /// Get the value spent by this input, by looking up its [`OutPoint`] in
    /// [`OrderedUtxo`]s.
    ///
    /// See [`Self::value`] for details.
    ///
    /// # Panics
    ///
    /// If the provided [`OrderedUtxo`]s don't have this input's [`OutPoint`].
    pub fn value_from_ordered_utxos(
        &self,
        ordered_utxos: &HashMap<OutPoint, utxo::OrderedUtxo>,
    ) -> Amount<NonNegative> {
        if let Some(outpoint) = self.outpoint() {
            // look up the specific Output and convert it to the expected format
            let output = ordered_utxos
                .get(&outpoint)
                .expect("provided Utxos don't have spent OutPoint")
                .utxo
                .output
                .clone();
            self.value_from_outputs(&iter::once((outpoint, output)).collect())
        } else {
            // coinbase inputs don't need any UTXOs
            self.value_from_outputs(&HashMap::new())
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
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary, Deserialize))]
#[cfg_attr(
    any(test, feature = "proptest-impl", feature = "elasticsearch"),
    derive(Serialize)
)]
pub struct Output {
    /// Transaction value.
    // At https://en.bitcoin.it/wiki/Protocol_documentation#tx, this is an i64.
    pub value: Amount<NonNegative>,

    /// The lock script defines the conditions under which this output can be spent.
    pub lock_script: Script,
}

impl Output {
    /// Returns a new [`Output`].
    pub fn new(amount: Amount<NonNegative>, lock_script: Script) -> Output {
        Output {
            value: amount,
            lock_script,
        }
    }

    /// Get the value contained in this output.
    /// This amount is subtracted from the transaction value pool by this output.
    pub fn value(&self) -> Amount<NonNegative> {
        self.value
    }

    /// Return the destination address from a transparent output.
    ///
    /// Returns None if the address type is not valid or unrecognized.
    pub fn address(&self, net: &Network) -> Option<Address> {
        match TxOut::try_from(self).ok()?.recipient_address()? {
            TransparentAddress::PublicKeyHash(pkh) => {
                Some(Address::from_pub_key_hash(net.t_addr_kind(), pkh))
            }
            TransparentAddress::ScriptHash(sh) => {
                Some(Address::from_script_hash(net.t_addr_kind(), sh))
            }
        }
    }

    /// Returns true if this output is considered dust.
    pub fn is_dust(&self) -> bool {
        let output_size: u32 = self
            .zcash_serialized_size()
            .try_into()
            .expect("output size should fit in u32");

        // https://github.com/zcash/zcash/blob/v6.10.0/src/primitives/transaction.cpp#L75-L80
        let threshold = 3 * (ONE_THIRD_DUST_THRESHOLD_RATE * (output_size + 148) / 1000);

        // https://github.com/zcash/zcash/blob/v6.10.0/src/primitives/transaction.h#L396-L399
        self.value.zatoshis() < threshold as i64
    }
}

/// A transparent output's index in its transaction.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct OutputIndex(u32);

impl OutputIndex {
    /// Create a transparent output index from the Zcash consensus integer type.
    ///
    /// `u32` is also the inner type.
    pub const fn from_index(output_index: u32) -> OutputIndex {
        OutputIndex(output_index)
    }

    /// Returns this index as the inner type.
    pub const fn index(&self) -> u32 {
        self.0
    }

    /// Create a transparent output index from `usize`.
    #[allow(dead_code)]
    pub fn from_usize(output_index: usize) -> OutputIndex {
        OutputIndex(
            output_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Return this index as `usize`.
    #[allow(dead_code)]
    pub fn as_usize(&self) -> usize {
        self.0
            .try_into()
            .expect("the maximum valid index fits in usize")
    }

    /// Create a transparent output index from `u64`.
    #[allow(dead_code)]
    pub fn from_u64(output_index: u64) -> OutputIndex {
        OutputIndex(
            output_index
                .try_into()
                .expect("the maximum u64 index fits in the inner type"),
        )
    }

    /// Return this index as `u64`.
    #[allow(dead_code)]
    pub fn as_u64(&self) -> u64 {
        self.0.into()
    }
}

impl AddAssign<u32> for OutputIndex {
    fn add_assign(&mut self, rhs: u32) {
        self.0 += rhs
    }
}
