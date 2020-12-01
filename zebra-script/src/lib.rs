//! Zebra script verification wrapping zcashd's zcash_script library
#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_script")]
// Disable some broken or unwanted clippy nightly lints
#![allow(clippy::unknown_clippy_lints)]
#![allow(clippy::unnecessary_wraps)]

use displaydoc::Display;
#[cfg(windows)]
use std::convert::TryInto;
use std::sync::Arc;
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

#[derive(Debug, Display, Error, PartialEq)]
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

/// Thin safe wrapper around ffi interface provided by libzcash_script
fn verify_script(
    script_pub_key: impl AsRef<[u8]>,
    amount: i64,
    tx_to: impl AsRef<[u8]>,
    n_in: u32,
    consensus_branch_id: u32,
) -> Result<(), Error> {
    let script_pub_key = script_pub_key.as_ref();
    let tx_to = tx_to.as_ref();

    let script_ptr = script_pub_key.as_ptr();
    let script_len = script_pub_key.len();
    let tx_to_ptr = tx_to.as_ptr();
    let tx_to_len = tx_to.len();
    let mut err = 0;

    let flags = zcash_script::zcash_script_SCRIPT_FLAGS_VERIFY_P2SH
        | zcash_script::zcash_script_SCRIPT_FLAGS_VERIFY_CHECKLOCKTIMEVERIFY;

    let ret = unsafe {
        zcash_script::zcash_script_verify(
            script_ptr,
            script_len as u32,
            amount,
            tx_to_ptr,
            tx_to_len as u32,
            n_in,
            #[cfg(not(windows))]
            flags,
            #[cfg(windows)]
            flags.try_into().expect("why bindgen whyyy"),
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

/// Verify a script within a transaction given the corresponding
/// `transparent::Output` it is spending and the `ConsensusBranchId` of the block
/// containing the transaction.
///
/// # Details
///
/// input index corresponds to the index of the `TransparentInput` which in
/// `transaction` used to identify the `previous_output`
pub fn is_valid(
    transaction: Arc<Transaction>,
    branch_id: ConsensusBranchId,
    (input_index, previous_output): (u32, transparent::Output),
) -> Result<(), Error> {
    assert!((input_index as usize) < transaction.inputs().len());

    let tx_to = transaction
        .zcash_serialize_to_vec()
        .expect("serialization into a vec is infallible");

    let transparent::Output { value, lock_script } = previous_output;

    verify_script(
        &lock_script.0,
        value.into(),
        &tx_to,
        input_index as _,
        branch_id.into(),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;
    use std::convert::TryInto;
    use std::sync::Arc;
    use zebra_chain::{
        parameters::NetworkUpgrade::*, serialization::ZcashDeserializeInto, transparent,
    };
    use zebra_test::prelude::*;

    lazy_static::lazy_static! {
        pub static ref SCRIPT_PUBKEY: Vec<u8> = <Vec<u8>>::from_hex("76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac")
            .unwrap();
        pub static ref SCRIPT_TX: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000")
            .expect("Block bytes are in valid hex representation");
    }

    #[test]
    fn verify_valid_script_parsed() -> Result<()> {
        zebra_test::init();

        let transaction =
            SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
        let coin = u64::pow(10, 8);
        let amount = 212 * coin;
        let output = transparent::Output {
            value: amount.try_into()?,
            lock_script: transparent::Script(SCRIPT_PUBKEY.clone()),
        };
        let input_index = 0;
        let branch_id = Blossom
            .branch_id()
            .expect("Blossom has a ConsensusBranchId");

        is_valid(transaction, branch_id, (input_index, output))?;

        Ok(())
    }

    #[test]
    fn verify_valid_script() -> Result<()> {
        zebra_test::init();

        let coin = i64::pow(10, 8);
        let script_pub_key = &*SCRIPT_PUBKEY;
        let amount = 212 * coin;
        let tx_to = &*SCRIPT_TX;
        let n_in = 0;
        let branch_id = 0x2bb40e60;

        verify_script(script_pub_key, amount, tx_to, n_in, branch_id)?;

        Ok(())
    }

    #[test]
    fn dont_verify_invalid_script() -> Result<()> {
        zebra_test::init();

        let coin = i64::pow(10, 8);
        let script_pub_key = &*SCRIPT_PUBKEY;
        let amount = 212 * coin;
        let tx_to = &*SCRIPT_TX;
        let n_in = 0;
        let branch_id = 0x2bb40e61;

        verify_script(script_pub_key, amount, tx_to, n_in, branch_id).unwrap_err();

        Ok(())
    }
}
