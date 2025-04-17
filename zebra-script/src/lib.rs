//! Zebra script verification wrapping zcashd's zcash_script library
#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_script")]
// We allow unsafe code, so we can call zcash_script
#![allow(unsafe_code)]

use core::fmt;
use std::{
    ffi::{c_int, c_uint, c_void},
    sync::Arc,
};

use thiserror::Error;

use zcash_script::{
    zcash_script_error_t, zcash_script_error_t_zcash_script_ERR_OK,
    zcash_script_error_t_zcash_script_ERR_TX_DESERIALIZE,
    zcash_script_error_t_zcash_script_ERR_TX_INDEX,
    zcash_script_error_t_zcash_script_ERR_TX_SIZE_MISMATCH,
};

use zebra_chain::{
    parameters::NetworkUpgrade,
    transaction::{HashType, SigHasher, Transaction},
    transparent,
};

/// An Error type representing the error codes returned from zcash_script.
#[derive(Copy, Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// script verification failed
    ScriptInvalid,
    /// could not deserialize tx
    TxDeserialize,
    /// input index out of bounds
    TxIndex,
    /// tx has an invalid size
    TxSizeMismatch,
    /// tx is a coinbase transaction and should not be verified
    TxCoinbase,
    /// unknown error from zcash_script: {0}
    Unknown(zcash_script_error_t),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            Error::ScriptInvalid => "script verification failed".to_owned(),
            Error::TxDeserialize => "could not deserialize tx".to_owned(),
            Error::TxIndex => "input index out of bounds".to_owned(),
            Error::TxSizeMismatch => "tx has an invalid size".to_owned(),
            Error::TxCoinbase => {
                "tx is a coinbase transaction and should not be verified".to_owned()
            }
            Error::Unknown(e) => format!("unknown error from zcash_script: {e}"),
        })
    }
}

impl From<zcash_script_error_t> for Error {
    #[allow(non_upper_case_globals)]
    fn from(err_code: zcash_script_error_t) -> Error {
        match err_code {
            zcash_script_error_t_zcash_script_ERR_OK => Error::ScriptInvalid,
            zcash_script_error_t_zcash_script_ERR_TX_DESERIALIZE => Error::TxDeserialize,
            zcash_script_error_t_zcash_script_ERR_TX_INDEX => Error::TxIndex,
            zcash_script_error_t_zcash_script_ERR_TX_SIZE_MISMATCH => Error::TxSizeMismatch,
            unknown => Error::Unknown(unknown),
        }
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
    all_previous_outputs: Vec<transparent::Output>,
}

/// A sighash context used for the zcash_script sighash callback.
struct SigHashContext<'a> {
    /// The index of the input being verified.
    input_index: usize,
    /// The SigHasher for the transaction being verified.
    sighasher: SigHasher<'a>,
}

/// The sighash callback to use with zcash_script.
extern "C" fn sighash(
    sighash_out: *mut u8,
    sighash_out_len: c_uint,
    ctx: *const c_void,
    script_code: *const u8,
    script_code_len: c_uint,
    hash_type: c_int,
) {
    // SAFETY: `ctx` is a valid SigHashContext because it is always passed to
    // `zcash_script_verify_callback` which simply forwards it to the callback.
    // `script_code` and `sighash_out` are valid buffers since they are always
    //  specified when the callback is called.
    unsafe {
        let ctx = ctx as *const SigHashContext;
        let script_code_vec =
            std::slice::from_raw_parts(script_code, script_code_len as usize).to_vec();
        let sighash = (*ctx).sighasher.sighash(
            HashType::from_bits_truncate(hash_type as u32),
            Some(((*ctx).input_index, script_code_vec)),
        );
        // Sanity check; must always be true.
        assert_eq!(sighash_out_len, sighash.0.len() as c_uint);
        std::ptr::copy_nonoverlapping(sighash.0.as_ptr(), sighash_out, sighash.0.len());
    }
}

impl CachedFfiTransaction {
    /// Construct a `PrecomputedTransaction` from a `Transaction` and the outputs
    /// from previous transactions that match each input in the transaction
    /// being verified.
    pub fn new(
        transaction: Arc<Transaction>,
        all_previous_outputs: Vec<transparent::Output>,
    ) -> Self {
        Self {
            transaction,
            all_previous_outputs,
        }
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

    /// Verify if the script in the input at `input_index` of a transaction correctly spends the
    /// matching [`transparent::Output`] it refers to.
    #[allow(clippy::unwrap_in_result)]
    pub fn is_valid(&self, nu: NetworkUpgrade, input_index: usize) -> Result<(), Error> {
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

        // This conversion is useful on some platforms, but not others.
        #[allow(clippy::useless_conversion)]
        let n_in = input_index
            .try_into()
            .expect("transaction indexes are much less than c_uint::MAX");

        let flags = zcash_script::zcash_script_SCRIPT_FLAGS_VERIFY_P2SH
            | zcash_script::zcash_script_SCRIPT_FLAGS_VERIFY_CHECKLOCKTIMEVERIFY;
        // This conversion is useful on some platforms, but not others.
        #[allow(clippy::useless_conversion)]
        let flags = flags
            .try_into()
            .expect("zcash_script_SCRIPT_FLAGS_VERIFY_* enum values fit in a c_uint");

        let mut err = 0;
        let lock_time = self.transaction.raw_lock_time() as i64;
        let is_final = if self.transaction.inputs()[input_index].sequence() == u32::MAX {
            1
        } else {
            0
        };
        let signature_script = match &self.transaction.inputs()[input_index] {
            transparent::Input::PrevOut {
                outpoint: _,
                unlock_script,
                sequence: _,
            } => unlock_script.as_raw_bytes(),
            transparent::Input::Coinbase { .. } => Err(Error::TxCoinbase)?,
        };

        let ctx = Box::new(SigHashContext {
            input_index: n_in,
            sighasher: SigHasher::new(&self.transaction, nu, &self.all_previous_outputs),
        });
        // SAFETY: The `script_*` fields are created from a valid Rust `slice`.
        let ret = unsafe {
            zcash_script::zcash_script_verify_callback(
                (&*ctx as *const SigHashContext) as *const c_void,
                Some(sighash),
                lock_time,
                is_final,
                script_pub_key.as_ptr(),
                script_pub_key.len() as u32,
                signature_script.as_ptr(),
                signature_script.len() as u32,
                flags,
                &mut err,
            )
        };

        if ret == 1 {
            Ok(())
        } else {
            Err(Error::from(err))
        }
    }

    /// Returns the number of transparent signature operations in the
    /// transparent inputs and outputs of this transaction.
    #[allow(clippy::unwrap_in_result)]
    pub fn legacy_sigop_count(&self) -> Result<u64, Error> {
        let mut count: u64 = 0;

        for input in self.transaction.inputs() {
            count += match input {
                transparent::Input::PrevOut {
                    outpoint: _,
                    unlock_script,
                    sequence: _,
                } => {
                    let script = unlock_script.as_raw_bytes();
                    // SAFETY: `script` is created from a valid Rust `slice`.
                    unsafe {
                        zcash_script::zcash_script_legacy_sigop_count_script(
                            script.as_ptr(),
                            script.len() as u32,
                        )
                    }
                }
                transparent::Input::Coinbase { .. } => 0,
            } as u64;
        }

        for output in self.transaction.outputs() {
            let script = output.lock_script.as_raw_bytes();
            // SAFETY: `script` is created from a valid Rust `slice`.
            let ret = unsafe {
                zcash_script::zcash_script_legacy_sigop_count_script(
                    script.as_ptr(),
                    script.len() as u32,
                )
            };
            count += ret as u64;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use hex::FromHex;
    use std::sync::Arc;
    use zebra_chain::{
        parameters::NetworkUpgrade,
        serialization::{ZcashDeserialize, ZcashDeserializeInto},
        transaction::Transaction,
        transparent::{self, Output},
    };
    use zebra_test::prelude::*;

    lazy_static::lazy_static! {
        pub static ref SCRIPT_PUBKEY: Vec<u8> = <Vec<u8>>::from_hex("76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac")
            .unwrap();
        pub static ref SCRIPT_TX: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000")
            .expect("Block bytes are in valid hex representation");
    }

    fn verify_valid_script(
        nu: NetworkUpgrade,
        tx: &[u8],
        amount: u64,
        pubkey: &[u8],
    ) -> Result<()> {
        let transaction =
            tx.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script::new(pubkey),
        };
        let input_index = 0;

        let previous_output = vec![output];
        let verifier = super::CachedFfiTransaction::new(transaction, previous_output);
        verifier.is_valid(nu, input_index)?;

        Ok(())
    }

    #[test]
    fn verify_valid_script_v4() -> Result<()> {
        let _init_guard = zebra_test::init();

        verify_valid_script(
            NetworkUpgrade::Blossom,
            &SCRIPT_TX,
            212 * u64::pow(10, 8),
            &SCRIPT_PUBKEY,
        )
    }

    #[test]
    fn count_legacy_sigops() -> Result<()> {
        let _init_guard = zebra_test::init();

        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

        let cached_tx = super::CachedFfiTransaction::new(transaction, Vec::new());
        assert_eq!(cached_tx.legacy_sigop_count()?, 1);

        Ok(())
    }

    #[test]
    fn fail_invalid_script() -> Result<()> {
        let _init_guard = zebra_test::init();

        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
        let coin = u64::pow(10, 8);
        let amount = 211 * coin;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()[..]),
        };
        let input_index = 0;
        let verifier = super::CachedFfiTransaction::new(transaction, vec![output]);
        verifier
            .is_valid(NetworkUpgrade::Blossom, input_index)
            .expect_err("verification should fail");

        Ok(())
    }

    #[test]
    fn reuse_script_verifier_pass_pass() -> Result<()> {
        let _init_guard = zebra_test::init();

        let coin = u64::pow(10, 8);
        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
        let amount = 212 * coin;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
        };

        let verifier = super::CachedFfiTransaction::new(transaction, vec![output]);

        let input_index = 0;

        verifier.is_valid(NetworkUpgrade::Blossom, input_index)?;
        verifier.is_valid(NetworkUpgrade::Blossom, input_index)?;

        Ok(())
    }

    #[test]
    fn reuse_script_verifier_pass_fail() -> Result<()> {
        let _init_guard = zebra_test::init();

        let coin = u64::pow(10, 8);
        let amount = 212 * coin;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
        };
        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

        let verifier = super::CachedFfiTransaction::new(transaction, vec![output]);

        let input_index = 0;

        verifier.is_valid(NetworkUpgrade::Blossom, input_index)?;
        verifier
            .is_valid(NetworkUpgrade::Blossom, input_index + 1)
            .expect_err("verification should fail");

        Ok(())
    }

    #[test]
    fn reuse_script_verifier_fail_pass() -> Result<()> {
        let _init_guard = zebra_test::init();

        let coin = u64::pow(10, 8);
        let amount = 212 * coin;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
        };
        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

        let verifier = super::CachedFfiTransaction::new(transaction, vec![output]);

        let input_index = 0;

        verifier
            .is_valid(NetworkUpgrade::Blossom, input_index + 1)
            .expect_err("verification should fail");
        verifier.is_valid(NetworkUpgrade::Blossom, input_index)?;

        Ok(())
    }

    #[test]
    fn reuse_script_verifier_fail_fail() -> Result<()> {
        let _init_guard = zebra_test::init();

        let coin = u64::pow(10, 8);
        let amount = 212 * coin;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
        };
        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

        let verifier = super::CachedFfiTransaction::new(transaction, vec![output]);

        let input_index = 0;

        verifier
            .is_valid(NetworkUpgrade::Blossom, input_index + 1)
            .expect_err("verification should fail");

        verifier
            .is_valid(NetworkUpgrade::Blossom, input_index + 1)
            .expect_err("verification should fail");

        Ok(())
    }

    #[test]
    fn p2sh() -> Result<()> {
        let _init_guard = zebra_test::init();

        // real tx with txid 51ded0b026f1ff56639447760bcd673b9f4e44a8afbf3af1dbaa6ca1fd241bea
        let serialized_tx = "0400008085202f8901c21354bf2305e474ad695382e68efc06e2f8b83c512496f615d153c2e00e688b00000000fdfd0000483045022100d2ab3e6258fe244fa442cfb38f6cef9ac9a18c54e70b2f508e83fa87e20d040502200eead947521de943831d07a350e45af8e36c2166984a8636f0a8811ff03ed09401473044022013e15d865010c257eef133064ef69a780b4bc7ebe6eda367504e806614f940c3022062fdbc8c2d049f91db2042d6c9771de6f1ef0b3b1fea76c1ab5542e44ed29ed8014c69522103b2cc71d23eb30020a4893982a1e2d352da0d20ee657fa02901c432758909ed8f21029d1e9a9354c0d2aee9ffd0f0cea6c39bbf98c4066cf143115ba2279d0ba7dabe2103e32096b63fd57f3308149d238dcbb24d8d28aad95c0e4e74e3e5e6a11b61bcc453aeffffffff0250954903000000001976a914a5a4e1797dac40e8ce66045d1a44c4a63d12142988acccf41c590000000017a9141c973c68b2acc6d6688eff9c7a9dd122ac1346ab8786c72400000000000000000000000000000000";
        let serialized_output = "4065675c0000000017a914c117756dcbe144a12a7c33a77cfa81aa5aeeb38187";
        let tx = Transaction::zcash_deserialize(&hex::decode(serialized_tx).unwrap().to_vec()[..])
            .unwrap();

        let previous_output =
            Output::zcash_deserialize(&hex::decode(serialized_output).unwrap().to_vec()[..])
                .unwrap();

        let verifier = super::CachedFfiTransaction::new(Arc::new(tx), vec![previous_output]);

        verifier.is_valid(NetworkUpgrade::Nu5, 0)?;

        Ok(())
    }
}
