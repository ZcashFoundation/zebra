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

use zcash_script::opcode::PossiblyBad;
use zcash_script::script::{self, Evaluable};
use zcash_script::{solver, Opcode};
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
                let script_code_vec = script_code.0.clone();
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

/// Trait for counting the number of transparent signature operations
/// in the transparent inputs and outputs of a transaction.
pub trait Sigops {
    /// Returns the number of transparent signature operations in the
    /// transparent inputs and outputs of the given transaction.
    fn sigops(&self) -> Result<u32, libzcash_script::Error> {
        let interpreter = get_interpreter(&|_, _| None, 0, true);

        Ok(self.scripts().try_fold(0, |acc, s| {
            interpreter
                .legacy_sigop_count_script(&script::Code(s.to_vec()))
                .map(|n| acc + n)
        })?)
    }

    /// Returns an iterator over the input and output scripts in the transaction.
    ///
    /// The number of input scripts in a coinbase tx is zero.
    fn scripts(&self) -> impl Iterator<Item = &[u8]>;
}

impl Sigops for zebra_chain::transaction::Transaction {
    fn scripts(&self) -> impl Iterator<Item = &[u8]> {
        self.inputs()
            .iter()
            .filter_map(|input| match input {
                transparent::Input::PrevOut { unlock_script, .. } => {
                    Some(unlock_script.as_raw_bytes())
                }
                transparent::Input::Coinbase { .. } => None,
            })
            .chain(self.outputs().iter().map(|o| o.lock_script.as_raw_bytes()))
    }
}

impl Sigops for zebra_chain::transaction::UnminedTx {
    fn scripts(&self) -> impl Iterator<Item = &[u8]> {
        self.transaction.scripts()
    }
}

impl Sigops for CachedFfiTransaction {
    fn scripts(&self) -> impl Iterator<Item = &[u8]> {
        self.transaction.scripts()
    }
}

/// Maximum sigops allowed in a P2SH redeemed script (zcashd MAX_P2SH_SIGOPS).
/// https://github.com/zcash/zcash/blob/v6.10.0/src/policy/policy.h
pub const MAX_P2SH_SIGOPS: u32 = 15;

/// Classify a script using zcashd's `Solver()`.
///
/// Returns `Some(kind)` for standard script types, `None` for non-standard.
fn classify_script(code: &script::Code) -> Option<solver::ScriptKind> {
    let component = code.to_component().ok()?.refine().ok()?;
    solver::standard(&component)
}

/// Extract the redeemed script bytes from a P2SH scriptSig.
///
/// The redeemed script is the last data push in the scriptSig.
/// Returns `None` if the scriptSig has no push operations.
fn extract_p2sh_redeemed_script(unlock_script: &transparent::Script) -> Option<Vec<u8>> {
    let code = script::Code(unlock_script.as_raw_bytes().to_vec());
    let mut last_push_data: Option<Vec<u8>> = None;
    for opcode in code.parse() {
        if let Ok(PossiblyBad::Good(Opcode::PushValue(pv))) = opcode {
            last_push_data = Some(pv.value());
        }
    }
    last_push_data
}

/// Count the number of push operations in a script.
///
/// For a push-only script (already enforced for mempool scriptSigs),
/// this equals the stack depth after evaluation.
fn count_script_push_ops(script_bytes: &[u8]) -> usize {
    let code = script::Code(script_bytes.to_vec());
    code.parse()
        .filter(|op| matches!(op, Ok(PossiblyBad::Good(Opcode::PushValue(_)))))
        .count()
}

/// Returns the expected number of scriptSig arguments for a given script kind.
///
/// Mirrors zcashd's `ScriptSigArgsExpected()`:
/// https://github.com/zcash/zcash/blob/v6.10.0/src/policy/policy.cpp#L108
///
/// Returns `None` for non-standard types (TX_NONSTANDARD, TX_NULL_DATA).
fn script_sig_args_expected(kind: &solver::ScriptKind) -> Option<usize> {
    match kind {
        solver::ScriptKind::PubKey { .. } => Some(1),
        solver::ScriptKind::PubKeyHash { .. } => Some(2),
        solver::ScriptKind::ScriptHash { .. } => Some(1),
        solver::ScriptKind::MultiSig { required, .. } => Some(*required as usize + 1),
        solver::ScriptKind::NullData { .. } => None,
    }
}

/// Extracts the P2SH redeemed script's sigop count for a single input.
///
/// Returns `Some(count)` for P2SH inputs where a redeemed script was found,
/// `None` for non-P2SH, coinbase, or P2SH inputs with an empty scriptSig.
fn p2sh_redeemed_script_sigop_count(
    input: &transparent::Input,
    spent_output: &transparent::Output,
) -> Option<u32> {
    let unlock_script = match input {
        transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
        transparent::Input::Coinbase { .. } => return None,
    };

    let lock_code = script::Code(spent_output.lock_script.as_raw_bytes().to_vec());
    if !lock_code.is_pay_to_script_hash() {
        return None;
    }

    let redeemed_bytes = extract_p2sh_redeemed_script(unlock_script)?;
    let redeemed = script::Code(redeemed_bytes);
    Some(redeemed.sig_op_count(true))
}

/// Returns the total number of P2SH sigops across all inputs of the transaction.
///
/// Mirrors zcashd's `GetP2SHSigOpCount()`:
/// https://github.com/zcash/zcash/blob/v6.10.0/src/main.cpp#L1342
///
/// # Panics
///
/// Panics in debug builds if `spent_outputs.len()` does not equal the number of
/// transparent inputs.
pub fn p2sh_sigop_count(
    tx: &zebra_chain::transaction::Transaction,
    spent_outputs: &[transparent::Output],
) -> u32 {
    debug_assert_eq!(
        tx.inputs().len(),
        spent_outputs.len(),
        "spent_outputs length must match inputs length"
    );

    tx.inputs()
        .iter()
        .zip(spent_outputs.iter())
        .filter_map(|(input, spent_output)| p2sh_redeemed_script_sigop_count(input, spent_output))
        .sum()
}

/// Returns `true` if all transparent inputs are standard.
///
/// Mirrors zcashd's `AreInputsStandard()`:
/// https://github.com/zcash/zcash/blob/v6.10.0/src/policy/policy.cpp#L137
///
/// For each input:
/// 1. The spent output's scriptPubKey must be a known standard type (via `Solver()`).
///    Non-standard scripts and OP_RETURN outputs are rejected.
/// 2. The scriptSig stack depth must match `ScriptSigArgsExpected()`.
/// 3. For P2SH inputs:
///    - If the redeemed script is standard, its expected args are added to the total.
///    - If the redeemed script is non-standard, it must have <= [`MAX_P2SH_SIGOPS`] sigops.
///
/// # Panics
///
/// Panics in debug builds if `spent_outputs.len()` does not equal the number of
/// transparent inputs.
pub fn are_inputs_standard(
    tx: &zebra_chain::transaction::Transaction,
    spent_outputs: &[transparent::Output],
) -> bool {
    debug_assert_eq!(
        tx.inputs().len(),
        spent_outputs.len(),
        "spent_outputs length must match inputs length"
    );

    for (input, spent_output) in tx.inputs().iter().zip(spent_outputs.iter()) {
        let unlock_script = match input {
            transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
            transparent::Input::Coinbase { .. } => continue,
        };

        // Step 1: Classify the spent output's scriptPubKey via Solver().
        let lock_code = script::Code(spent_output.lock_script.as_raw_bytes().to_vec());
        let script_kind = match classify_script(&lock_code) {
            Some(kind) => kind,
            None => return false,
        };

        // Step 2: Get expected number of scriptSig arguments.
        // Returns None for TX_NONSTANDARD and TX_NULL_DATA.
        let mut n_args_expected = match script_sig_args_expected(&script_kind) {
            Some(n) => n,
            None => return false,
        };

        // Step 3: Count actual push operations in scriptSig.
        // For push-only scripts (enforced by IsStandardTx), this equals the stack depth.
        let stack_size = count_script_push_ops(unlock_script.as_raw_bytes());

        // Step 4: P2SH-specific checks.
        if matches!(script_kind, solver::ScriptKind::ScriptHash { .. }) {
            let Some(redeemed_bytes) = extract_p2sh_redeemed_script(unlock_script) else {
                return false;
            };

            let redeemed_code = script::Code(redeemed_bytes);

            match classify_script(&redeemed_code) {
                Some(ref inner_kind) => {
                    // Standard redeemed script: add its expected args.
                    match script_sig_args_expected(inner_kind) {
                        Some(inner) => n_args_expected += inner,
                        None => return false,
                    }
                }
                None => {
                    // Non-standard redeemed script: accept if sigops <= limit.
                    // Matches zcashd: "Any other Script with less than 15 sigops OK:
                    // ... extra data left on the stack after execution is OK, too"
                    return redeemed_code.sig_op_count(true) <= MAX_P2SH_SIGOPS;
                }
            }
        }

        // Step 5: Reject if scriptSig has wrong number of stack items.
        if stack_size != n_args_expected {
            return false;
        }
    }
    true
}

impl Sigops for zcash_primitives::transaction::Transaction {
    fn scripts(&self) -> impl Iterator<Item = &[u8]> {
        self.transparent_bundle().into_iter().flat_map(|bundle| {
            (!bundle.is_coinbase())
                .then(|| bundle.vin.iter().map(|i| i.script_sig().0 .0.as_slice()))
                .into_iter()
                .flatten()
                .chain(
                    bundle
                        .vout
                        .iter()
                        .map(|o| o.script_pubkey().0 .0.as_slice()),
                )
        })
    }
}
