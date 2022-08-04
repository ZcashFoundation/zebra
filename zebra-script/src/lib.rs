//! Zebra script verification wrapping zcashd's zcash_script library
#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_script")]
// We allow unsafe code, so we can call zcash_script
#![allow(unsafe_code)]

use std::sync::Arc;

use displaydoc::Display;
use thiserror::Error;

use zcash_script::{
    zcash_script_error_t, zcash_script_error_t_zcash_script_ERR_OK,
    zcash_script_error_t_zcash_script_ERR_TX_DESERIALIZE,
    zcash_script_error_t_zcash_script_ERR_TX_INDEX,
    zcash_script_error_t_zcash_script_ERR_TX_SIZE_MISMATCH,
};

use zebra_chain::{
    parameters::ConsensusBranchId, serialization::ZcashSerialize, transaction::Transaction,
    transparent,
};

#[derive(Copy, Clone, Debug, Display, Error, PartialEq, Eq)]
#[non_exhaustive]
/// An Error type representing the error codes returned from zcash_script.
pub enum Error {
    /// script failed to verify
    #[non_exhaustive]
    ScriptInvalid,
    /// could not to deserialize tx
    #[non_exhaustive]
    TxDeserialize,
    /// input index out of bounds for transaction's inputs
    #[non_exhaustive]
    TxIndex,
    /// tx is an invalid size for it's protocol
    #[non_exhaustive]
    TxSizeMismatch,
    /// encountered unknown error kind from zcash_script: {0}
    #[non_exhaustive]
    Unknown(zcash_script_error_t),
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
    /// This field is private so that `transaction`, `all_previous_outputs`, and `precomputed` always match.
    transaction: Arc<Transaction>,

    /// The outputs from previous transactions that match each input in the transaction
    /// being verified.
    ///
    /// SAFETY: this field must be private,
    ///         and `CachedFfiTransaction::new` must be the only method that modifies it,
    ///         so that it is [`Send`], [`Sync`], consistent with `transaction` and `precomputed`.
    all_previous_outputs: Vec<transparent::Output>,

    /// The deserialized `zcash_script` transaction, as a C++ object.
    ///
    /// SAFETY: this field must be private,
    ///         and `CachedFfiTransaction::new` must be the only method that modifies it,
    ///         so that it is [`Send`], [`Sync`], valid, and not NULL.
    precomputed: *mut std::ffi::c_void,
}

impl CachedFfiTransaction {
    /// Construct a `PrecomputedTransaction` from a `Transaction` and the outputs
    /// from previous transactions that match each input in the transaction
    /// being verified.
    pub fn new(
        transaction: Arc<Transaction>,
        all_previous_outputs: Vec<transparent::Output>,
    ) -> Self {
        let tx_to_serialized = transaction
            .zcash_serialize_to_vec()
            .expect("serialization into a vec is infallible");

        let tx_to_serialized_ptr = tx_to_serialized.as_ptr();
        let tx_to_serialized_len = tx_to_serialized
            .len()
            .try_into()
            .expect("serialized transaction lengths are much less than u32::MAX");
        let mut err = 0;

        let all_previous_outputs_serialized = all_previous_outputs
            .zcash_serialize_to_vec()
            .expect("serialization into a vec is infallible");
        let all_previous_outputs_serialized_ptr = all_previous_outputs_serialized.as_ptr();
        let all_previous_outputs_serialized_len: u32 = all_previous_outputs_serialized
            .len()
            .try_into()
            .expect("serialized transaction lengths are much less than u32::MAX");

        // SAFETY:
        // the `tx_to_*` fields are created from a valid Rust `Vec`
        // the `all_previous_outputs_*` fields are created from a valid Rust `Vec`
        let precomputed = unsafe {
            zcash_script::zcash_script_new_precomputed_tx_v5(
                tx_to_serialized_ptr,
                tx_to_serialized_len,
                all_previous_outputs_serialized_ptr,
                all_previous_outputs_serialized_len,
                &mut err,
            )
        };
        // SAFETY: the safety of other methods depends on `precomputed` being valid and not NULL.
        assert!(
            !precomputed.is_null(),
            "zcash_script_new_precomputed_tx returned {} ({})",
            err,
            Error::from(err)
        );

        Self {
            transaction,
            all_previous_outputs,
            // SAFETY: `precomputed` must not be modified after initialisation,
            //          so that it is `Send` and `Sync`.
            precomputed,
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

    /// Verify if the script in the input at `input_index` of a transaction correctly
    /// spends the matching [`transparent::Output`] it refers to, with the [`ConsensusBranchId`]
    /// of the block containing the transaction.
    #[allow(clippy::unwrap_in_result)]
    pub fn is_valid(&self, branch_id: ConsensusBranchId, input_index: usize) -> Result<(), Error> {
        let previous_output = self
            .all_previous_outputs
            .get(input_index)
            .ok_or(Error::TxIndex)?
            .clone();
        let transparent::Output { value, lock_script } = previous_output;
        let script_pub_key: &[u8] = lock_script.as_raw_bytes();

        // This conversion is useful on some platforms, but not others.
        #[allow(clippy::useless_conversion)]
        let n_in = input_index
            .try_into()
            .expect("transaction indexes are much less than c_uint::MAX");

        let script_ptr = script_pub_key.as_ptr();
        let script_len = script_pub_key.len();

        let amount = value.into();

        let flags = zcash_script::zcash_script_SCRIPT_FLAGS_VERIFY_P2SH
            | zcash_script::zcash_script_SCRIPT_FLAGS_VERIFY_CHECKLOCKTIMEVERIFY;
        // This conversion is useful on some platforms, but not others.
        #[allow(clippy::useless_conversion)]
        let flags = flags
            .try_into()
            .expect("zcash_script_SCRIPT_FLAGS_VERIFY_* enum values fit in a c_uint");

        let consensus_branch_id = branch_id.into();

        let mut err = 0;

        // SAFETY: `CachedFfiTransaction::new` makes sure `self.precomputed` is not NULL.
        //         The `script_*` fields are created from a valid Rust `slice`.
        let ret = unsafe {
            zcash_script::zcash_script_verify_precomputed(
                self.precomputed,
                n_in,
                script_ptr,
                script_len
                    .try_into()
                    .expect("script lengths are much less than u32::MAX"),
                amount,
                flags,
                consensus_branch_id,
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
        let mut err = 0;

        // SAFETY: `CachedFfiTransaction::new` makes sure `self.precomputed` is not NULL.
        let ret = unsafe {
            zcash_script::zcash_script_legacy_sigop_count_precomputed(self.precomputed, &mut err)
        };

        if err == zcash_script_error_t_zcash_script_ERR_OK {
            let ret = ret.try_into().expect("c_uint fits in a u64");
            Ok(ret)
        } else {
            Err(Error::from(err))
        }
    }
}

// # SAFETY
//
// ## Justification
//
// `CachedFfiTransaction` is not `Send` and `Sync` by default because of the
// `*mut c_void` it contains. This is because raw pointers could allow the same
// data to be mutated from different threads if copied.
//
// CachedFFiTransaction needs to be Send and Sync to be stored within a `Box<dyn
// Future + Send + Sync + static>`. In `zebra_consensus/src/transaction.rs`, an
// async block owns a `CachedFfiTransaction`, and holds it across an await
// point, while the transaction verifier is spawning all of the script verifier
// futures. The service readiness check requires this await between each task
// spawn. Each `script` future needs a copy of the
// `Arc<CachedFfiTransaction>` so that it can simultaneously verify inputs
// without cloning the c++ allocated type unnecessarily.
//
// ## Explanation
//
// It is safe for us to mark this as `Send` and `Sync` because the data pointed
// to by `precomputed` is never modified after it is constructed and points to
// heap memory with a stable memory location. The function
// `zcash_script::zcash_script_verify_precomputed` only reads from the
// precomputed context while verifying inputs, which makes it safe to treat this
// pointer like a shared reference (given that is how it is used).
//
// The function `zcash_script:zcash_script_legacy_sigop_count_precomputed` only reads
// from the precomputed context. Currently, these reads happen after all the concurrent
// async checks have finished.
//
// Since we're manually marking it as `Send` and `Sync`, we must ensure that
// other fields in the struct are also `Send` and `Sync`. This applies to
// `all_previous_outputs`, which are both.
//
// TODO: create a wrapper for `precomputed` and only make it implement Send/Sync (#3436)
unsafe impl Send for CachedFfiTransaction {}
unsafe impl Sync for CachedFfiTransaction {}

impl Drop for CachedFfiTransaction {
    fn drop(&mut self) {
        // SAFETY: `CachedFfiTransaction::new` makes sure `self.precomputed` is not NULL.
        unsafe { zcash_script::zcash_script_free_precomputed_tx(self.precomputed) };
    }
}

#[cfg(test)]
mod tests {
    use hex::FromHex;
    use std::convert::TryInto;
    use std::sync::Arc;
    use zebra_chain::{
        parameters::{ConsensusBranchId, NetworkUpgrade::*},
        serialization::ZcashDeserializeInto,
        transparent,
    };
    use zebra_test::prelude::*;

    lazy_static::lazy_static! {
        pub static ref SCRIPT_PUBKEY: Vec<u8> = <Vec<u8>>::from_hex("76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac")
            .unwrap();
        pub static ref SCRIPT_TX: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000")
            .expect("Block bytes are in valid hex representation");
    }

    fn verify_valid_script(
        branch_id: ConsensusBranchId,
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
        verifier.is_valid(branch_id, input_index)?;

        Ok(())
    }

    #[test]
    fn verify_valid_script_v4() -> Result<()> {
        let _init_guard = zebra_test::init();

        verify_valid_script(
            Blossom.branch_id().unwrap(),
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
        let branch_id = Blossom
            .branch_id()
            .expect("Blossom has a ConsensusBranchId");

        let verifier = super::CachedFfiTransaction::new(transaction, vec![output]);
        verifier.is_valid(branch_id, input_index).unwrap_err();

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
        let branch_id = Blossom
            .branch_id()
            .expect("Blossom has a ConsensusBranchId");

        verifier.is_valid(branch_id, input_index)?;

        verifier.is_valid(branch_id, input_index)?;

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
        let branch_id = Blossom
            .branch_id()
            .expect("Blossom has a ConsensusBranchId");

        verifier.is_valid(branch_id, input_index)?;

        verifier.is_valid(branch_id, input_index + 1).unwrap_err();

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
        let branch_id = Blossom
            .branch_id()
            .expect("Blossom has a ConsensusBranchId");

        verifier.is_valid(branch_id, input_index + 1).unwrap_err();

        verifier.is_valid(branch_id, input_index)?;

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
        let branch_id = Blossom
            .branch_id()
            .expect("Blossom has a ConsensusBranchId");

        verifier.is_valid(branch_id, input_index + 1).unwrap_err();

        verifier.is_valid(branch_id, input_index + 1).unwrap_err();

        Ok(())
    }
}
