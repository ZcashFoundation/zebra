//! Zebra script verification wrapping zcashd's zcash_script library
#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_script")]
// We allow unsafe code, so we can call zcash_script
#![allow(unsafe_code)]

#[cfg(test)]
mod tests;

use core::fmt;
use std::sync::Arc;

use thiserror::Error;

use libzcash_script::ZcashScript;

use zcash_script::{opcode::PossiblyBad, script, script::Evaluable as _, Opcode};
use zebra_chain::{
    parameters::NetworkUpgrade,
    transaction::{HashType, SigHasher},
    transparent,
};

/// An Error type representing the error codes returned from zcash_script.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// script verification failed
    ScriptInvalid,
    /// input index out of bounds
    TxIndex,
    /// tx is a coinbase transaction and should not be verified
    TxCoinbase,
    /// unknown error from zcash_script: {0}
    Unknown(libzcash_script::Error),
    /// transaction is invalid according to zebra_chain (not a zcash_script error)
    TxInvalid(#[from] zebra_chain::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            Error::ScriptInvalid => "script verification failed".to_owned(),
            Error::TxIndex => "input index out of bounds".to_owned(),
            Error::TxCoinbase => {
                "tx is a coinbase transaction and should not be verified".to_owned()
            }
            Error::Unknown(e) => format!("unknown error from zcash_script: {e:?}"),
            Error::TxInvalid(e) => format!("tx is invalid: {e}"),
        })
    }
}

impl From<libzcash_script::Error> for Error {
    #[allow(non_upper_case_globals)]
    fn from(err_code: libzcash_script::Error) -> Error {
        Error::Unknown(err_code)
    }
}

/// Get the interpreter according to the feature flag
fn get_interpreter(
    sighash: zcash_script::interpreter::SighashCalculator<'_>,
    lock_time: u32,
    is_final: bool,
) -> impl ZcashScript + use<'_> {
    #[cfg(feature = "comparison-interpreter")]
    return libzcash_script::cxx_rust_comparison_interpreter(sighash, lock_time, is_final);
    #[cfg(not(feature = "comparison-interpreter"))]
    libzcash_script::CxxInterpreter {
        sighash,
        lock_time,
        is_final,
    }
}

/// A preprocessed Transaction which can be used to verify scripts within said
/// Transaction.
#[derive(Debug)]
pub struct CachedFfiTransaction {
    /// The deserialized Zebra transaction.
    ///
    /// This field is private so that `transaction`, and `all_previous_outputs` always match.
    transaction: Arc<zebra_chain::transaction::Transaction>,

    /// The outputs from previous transactions that match each input in the transaction
    /// being verified.
    all_previous_outputs: Arc<Vec<transparent::Output>>,

    /// The sighasher context to use to compute sighashes.
    sighasher: SigHasher,
}

impl CachedFfiTransaction {
    /// Construct a `CachedFfiTransaction` from a `Transaction` and the outputs
    /// from previous transactions that match each input in the transaction
    /// being verified.
    pub fn new(
        transaction: Arc<zebra_chain::transaction::Transaction>,
        all_previous_outputs: Arc<Vec<transparent::Output>>,
        nu: NetworkUpgrade,
    ) -> Result<Self, Error> {
        let sighasher = transaction.sighasher(nu, all_previous_outputs.clone())?;
        Ok(Self {
            transaction,
            all_previous_outputs,
            sighasher,
        })
    }

    /// Returns the transparent inputs for this transaction.
    pub fn inputs(&self) -> &[transparent::Input] {
        self.transaction.inputs()
    }

    /// Returns the outputs from previous transactions that match each input in the transaction
    /// being verified.
    pub fn all_previous_outputs(&self) -> &Vec<transparent::Output> {
        &self.all_previous_outputs
    }

    /// Return the sighasher being used for this transaction.
    pub fn sighasher(&self) -> &SigHasher {
        &self.sighasher
    }

    /// Returns the total number of P2SH sigops across all inputs of this transaction.
    ///
    /// Mirrors zcashd's [`GetP2SHSigOpCount()`].
    ///
    /// For each P2SH input (where the spent `scriptPubKey` is P2SH), the redeem script (the last
    /// data push in the `scriptSig`) is parsed in "accurate" mode and its sigops are counted.
    /// Coinbase inputs contribute zero.
    ///
    /// This must be included in the block-wide `MAX_BLOCK_SIGOPS` total to match zcashd's consensus
    /// behavior.
    ///
    /// [`GetP2SHSigOpCount()`]: https://github.com/zcash/zcash/blob/v6.11.0/src/main.cpp#L840-L852
    pub fn p2sh_sigops(&self) -> u32 {
        p2sh_sigop_count(&self.transaction, &self.all_previous_outputs)
    }

    /// Verify if the script in the input at `input_index` of a transaction correctly spends the
    /// matching [`transparent::Output`] it refers to.
    #[allow(clippy::unwrap_in_result)]
    pub fn is_valid(&self, input_index: usize) -> Result<(), Error> {
        let previous_output = self
            .all_previous_outputs
            .get(input_index)
            .ok_or(Error::TxIndex)?
            .clone();
        let transparent::Output {
            value: _,
            lock_script,
        } = previous_output;
        let script_pub_key: &[u8] = lock_script.as_raw_bytes();

        let flags = zcash_script::interpreter::Flags::P2SH
            | zcash_script::interpreter::Flags::CHECKLOCKTIMEVERIFY;

        let lock_time = self.transaction.raw_lock_time();
        let is_final = self.transaction.inputs()[input_index].sequence() == u32::MAX;
        let signature_script = match &self.transaction.inputs()[input_index] {
            transparent::Input::PrevOut {
                outpoint: _,
                unlock_script,
                sequence: _,
            } => unlock_script.as_raw_bytes(),
            transparent::Input::Coinbase { .. } => Err(Error::TxCoinbase)?,
        };

        let script =
            script::Raw::from_raw_parts(signature_script.to_vec(), script_pub_key.to_vec());

        let calculate_sighash =
            |script_code: &script::Code, hash_type: &zcash_script::signature::HashType| {
                // For v5+ transactions: reject undefined hash_type values,
                // matching zcashd's SighashType::parse behavior.
                // Valid values: {0x01, 0x02, 0x03, 0x81, 0x82, 0x83}.
                if self.transaction.version() >= 5 {
                    let valid_v5_types: &[i32] = &[0x01, 0x02, 0x03, 0x81, 0x82, 0x83];
                    if !valid_v5_types.contains(&hash_type.raw_bits()) {
                        return None;
                    }
                }

                let script_code_vec = script_code.0.clone();

                // For pre-v5 (v4) transactions: zcashd serializes the raw
                // hash_type byte into the sighash preimage (only masking with
                // 0x1f for selection logic). Use the raw byte to match.
                if self.transaction.version() < 5 {
                    let raw_byte = hash_type.raw_bits() as u8;
                    return Some(
                        self.sighasher()
                            .sighash_v4_raw(raw_byte, Some((input_index, script_code_vec)))
                            .0,
                    );
                }

                let mut our_hash_type = match hash_type.signed_outputs() {
                    zcash_script::signature::SignedOutputs::All => HashType::ALL,
                    zcash_script::signature::SignedOutputs::Single => HashType::SINGLE,
                    zcash_script::signature::SignedOutputs::None => HashType::NONE,
                };
                if hash_type.anyone_can_pay() {
                    our_hash_type |= HashType::ANYONECANPAY;
                }
                Some(
                    self.sighasher()
                        .sighash(our_hash_type, Some((input_index, script_code_vec)))
                        .0,
                )
            };
        let interpreter = get_interpreter(&calculate_sighash, lock_time, is_final);
        interpreter
            .verify_callback(&script, flags)
            .map_err(|(_, e)| Error::from(e))
            .and_then(|res| {
                if res {
                    Ok(())
                } else {
                    Err(Error::ScriptInvalid)
                }
            })
    }
}

/// Trait for counting the number of transparent signature operations in the transparent inputs and
/// outputs of a transaction.
///
/// Mirrors zcashd's [`GetLegacySigOpCount()`].
///
/// All transparent inputs are included, including the coinbase input script. zcashd charges
/// coinbase `scriptSig` sigops against the block `MAX_BLOCK_SIGOPS` limit, so Zebra must do the
/// same to avoid a consensus split.
///
/// [`GetLegacySigOpCount()`]: https://github.com/zcash/zcash/blob/v6.11.0/src/main.cpp#L826-L836
pub trait Sigops {
    /// Returns the number of transparent signature operations in the
    /// transparent inputs and outputs of the given transaction.
    fn sigops(&self) -> Result<u32, libzcash_script::Error> {
        let interpreter = get_interpreter(&|_, _| None, 0, true);

        Ok(self.scripts().try_fold(0, |acc, s| {
            interpreter
                .legacy_sigop_count_script(&script::Code(s))
                .map(|n| acc + n)
        })?)
    }

    /// Returns an iterator over the input and output scripts in the transaction.
    ///
    /// For consensus sigop accounting, this must include the coinbase input
    /// script (height prefix followed by extra data), matching zcashd's
    /// `GetLegacySigOpCount()`.
    fn scripts(&self) -> impl Iterator<Item = Vec<u8>>;
}

impl Sigops for zebra_chain::transaction::Transaction {
    fn scripts(&self) -> impl Iterator<Item = Vec<u8>> {
        self.inputs()
            .iter()
            .map(|input| match input {
                transparent::Input::PrevOut { unlock_script, .. } => {
                    unlock_script.as_raw_bytes().to_vec()
                }
                // Coinbase scriptSig = encoded height || extra data, which must be reconstructed
                // for sigop counting. `coinbase_script()` round-trips through
                // `write_coinbase_height`, which only fails when called on a malformed in-memory
                // genesis coinbase. Any coinbase that was successfully deserialized round-trips
                // cleanly, so this `expect` cannot fire on validation paths.
                transparent::Input::Coinbase { .. } => input
                    .coinbase_script()
                    .expect("coinbase_script reconstructs from a deserialized coinbase input"),
            })
            .chain(
                self.outputs()
                    .iter()
                    .map(|o| o.lock_script.as_raw_bytes().to_vec()),
            )
    }
}

impl Sigops for zebra_chain::transaction::UnminedTx {
    fn scripts(&self) -> impl Iterator<Item = Vec<u8>> {
        self.transaction.scripts()
    }
}

impl Sigops for CachedFfiTransaction {
    fn scripts(&self) -> impl Iterator<Item = Vec<u8>> {
        self.transaction.scripts()
    }
}

impl Sigops for zcash_primitives::transaction::Transaction {
    fn scripts(&self) -> impl Iterator<Item = Vec<u8>> {
        self.transparent_bundle().into_iter().flat_map(|bundle| {
            // `zcash_primitives` stores the coinbase input's full serialized scriptSig (height
            // prefix + extra data) in the synthesized input's script_sig, so it is included as-is
            // for sigop counting.
            bundle
                .vin
                .iter()
                .map(|i| i.script_sig().0 .0.clone())
                .chain(bundle.vout.iter().map(|o| o.script_pubkey().0 .0.clone()))
        })
    }
}

/// Extract the redeem script bytes from a P2SH scriptSig.
///
/// Mirrors zcashd's P2SH redeem-script extraction in
/// [`CScript::GetSigOpCount(const CScript& scriptSig)`].
///
/// Iterates the scriptSig opcodes and returns the last successfully pushed data value. Returns
/// `None` if any opcode fails to parse, OR if any opcode is not a push value (zcashd: `opcode >
/// OP_16`). This matches zcashd's behavior of returning 0 P2SH sigops for malformed or
/// non-push-only scriptSigs.
///
/// [`CScript::GetSigOpCount(const CScript& scriptSig)`]: https://github.com/zcash/zcash/blob/v6.11.0/src/script/script.cpp#L176-L199
fn extract_p2sh_redeem_script(unlock_script: &transparent::Script) -> Option<Vec<u8>> {
    let code = script::Code(unlock_script.as_raw_bytes().to_vec());
    let mut last_push_data: Option<Vec<u8>> = None;
    for opcode in code.parse() {
        match opcode {
            Ok(PossiblyBad::Good(Opcode::PushValue(pv))) => {
                last_push_data = Some(pv.value());
            }
            // Non-push opcode (operation, control, or bad) or parse error: zcashd returns 0 sigops
            // in this case. Match that behavior by discarding any data collected so far.
            _ => return None,
        }
    }
    last_push_data
}

/// Returns the P2SH sigop count for a single input.
///
/// Returns 0 for non-P2SH inputs, coinbase inputs, and P2SH inputs where no redeem script can be
/// extracted from the scriptSig.
fn p2sh_input_sigop_count(input: &transparent::Input, spent_output: &transparent::Output) -> u32 {
    let unlock_script = match input {
        transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
        transparent::Input::Coinbase { .. } => return 0,
    };

    let lock_code = script::Code(spent_output.lock_script.as_raw_bytes().to_vec());

    if !lock_code.is_pay_to_script_hash() {
        return 0;
    }

    let Some(redeemed_bytes) = extract_p2sh_redeem_script(unlock_script) else {
        return 0;
    };

    script::Code(redeemed_bytes).sig_op_count(true)
}

/// Returns the total number of P2SH sigops across all inputs of `tx`.
///
/// Mirrors zcashd's [`GetP2SHSigOpCount()`].
///
/// Coinbase transactions always return zero, matching zcashd's early-return for `tx.IsCoinBase()`.
/// Callers are therefore permitted to pass an empty `spent_outputs` slice for coinbase transactions
/// (which is what the block-verifier does, since coinbase inputs have no previous output).
///
/// # Correctness
///
/// For non-coinbase transactions, `spent_outputs.len()` must equal the number of transparent inputs
/// in `tx`. If the lengths differ, `zip()` silently truncates the longer iterator, causing an
/// incorrect (undercount) result.
///
/// [`GetP2SHSigOpCount()`]: https://github.com/zcash/zcash/blob/v6.11.0/src/main.cpp#L840-L852
pub fn p2sh_sigop_count(
    tx: &zebra_chain::transaction::Transaction,
    spent_outputs: &[transparent::Output],
) -> u32 {
    if tx.is_coinbase() {
        return 0;
    }

    debug_assert_eq!(
        tx.inputs().len(),
        spent_outputs.len(),
        "spent_outputs must align with transaction inputs for non-coinbase txs"
    );

    tx.inputs()
        .iter()
        .zip(spent_outputs.iter())
        .map(|(input, spent_output)| p2sh_input_sigop_count(input, spent_output))
        .sum()
}
