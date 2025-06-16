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

use zcash_script::ZcashScript;

use zebra_chain::{
    parameters::NetworkUpgrade,
    transaction::{HashType, SigHasher, Transaction},
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
    Unknown(zcash_script::Error),
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

impl From<zcash_script::Error> for Error {
    #[allow(non_upper_case_globals)]
    fn from(err_code: zcash_script::Error) -> Error {
        match err_code {
            zcash_script::Error::Ok(_) => Error::ScriptInvalid,
            unknown => Error::Unknown(unknown),
        }
    }
}

/// Get the interpreter according to the feature flag
fn get_interpreter(
    sighash: zcash_script::SighashCalculator,
    lock_time: u32,
    is_final: bool,
    #[allow(unused)] flags: zcash_script::VerificationFlags,
) -> impl ZcashScript + use<'_> {
    #[cfg(feature = "comparison-interpreter")]
    return zcash_script::cxx_rust_comparison_interpreter(sighash, lock_time, is_final, flags);
    #[cfg(not(feature = "comparison-interpreter"))]
    zcash_script::CxxInterpreter {
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
    transaction: Arc<Transaction>,

    /// The outputs from previous transactions that match each input in the transaction
    /// being verified.
    all_previous_outputs: Arc<Vec<transparent::Output>>,

    /// The sighasher context to use to compute sighashes.
    sighasher: SigHasher,
}

impl CachedFfiTransaction {
    /// Construct a `PrecomputedTransaction` from a `Transaction` and the outputs
    /// from previous transactions that match each input in the transaction
    /// being verified.
    pub fn new(
        transaction: Arc<Transaction>,
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

        let flags = zcash_script::VerificationFlags::P2SH
            | zcash_script::VerificationFlags::CHECKLOCKTIMEVERIFY;

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

        let calculate_sighash = |script_code: &[u8], hash_type: zcash_script::HashType| {
            let script_code_vec = script_code.to_vec();
            let mut our_hash_type = match hash_type.signed_outputs {
                zcash_script::SignedOutputs::All => HashType::ALL,
                zcash_script::SignedOutputs::Single => HashType::SINGLE,
                zcash_script::SignedOutputs::None => HashType::NONE,
            };
            if hash_type.anyone_can_pay {
                our_hash_type |= HashType::ANYONECANPAY;
            }
            Some(
                self.sighasher()
                    .sighash(our_hash_type, Some((input_index, script_code_vec)))
                    .0,
            )
        };
        let interpreter = get_interpreter(&calculate_sighash, lock_time, is_final, flags);
        interpreter
            .verify_callback(script_pub_key, signature_script, flags)
            .map_err(Error::from)
    }
}

/// Returns the number of transparent signature operations in the
/// transparent inputs and outputs of the given transaction.
#[allow(clippy::unwrap_in_result)]
pub fn legacy_sigop_count(transaction: &Transaction) -> Result<u64, Error> {
    let mut count: u64 = 0;

    // Create a dummy interpreter since these inputs are not used to count
    // the sigops
    let interpreter = get_interpreter(
        &|_, _| None,
        0,
        true,
        zcash_script::VerificationFlags::P2SH
            | zcash_script::VerificationFlags::CHECKLOCKTIMEVERIFY,
    );

    for input in transaction.inputs() {
        count += match input {
            transparent::Input::PrevOut {
                outpoint: _,
                unlock_script,
                sequence: _,
            } => {
                let script = unlock_script.as_raw_bytes();
                interpreter
                    .legacy_sigop_count_script(script)
                    .map_err(Error::from)?
            }
            transparent::Input::Coinbase { .. } => 0,
        } as u64;
    }

    for output in transaction.outputs() {
        let script = output.lock_script.as_raw_bytes();
        let ret = interpreter
            .legacy_sigop_count_script(script)
            .map_err(Error::from)?;
        count += ret as u64;
    }
    Ok(count)
}

/// Trait for counting the number of transparent signature operations
/// in the transparent inputs and outputs of a transaction.
pub trait Sigops {
    /// Returns the number of transparent signature operations in the
    /// transparent inputs and outputs of the given transaction.
    fn sigops(&self) -> Result<u32, zcash_script::Error> {
        let interpreter = get_interpreter(
            &|_, _| None,
            0,
            true,
            zcash_script::VerificationFlags::P2SH
                | zcash_script::VerificationFlags::CHECKLOCKTIMEVERIFY,
        );

        self.scripts().try_fold(0, |acc, s| {
            interpreter.legacy_sigop_count_script(s).map(|n| acc + n)
        })
    }

    /// Returns an iterator over the input and output scripts in the transaction.
    ///
    /// The number of input scripts in a coinbase tx is zero.
    fn scripts(&self) -> impl Iterator<Item = &[u8]>;
}

