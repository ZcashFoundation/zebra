#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_script")]

use displaydoc::Display;
use std::sync::Arc;
use thiserror::Error;
use zcashconsensus::{
    zcashconsensus_error_t, zcashconsensus_error_t_zcashconsensus_ERR_OK,
    zcashconsensus_error_t_zcashconsensus_ERR_TX_DESERIALIZE,
    zcashconsensus_error_t_zcashconsensus_ERR_TX_INDEX,
    zcashconsensus_error_t_zcashconsensus_ERR_TX_SIZE_MISMATCH,
};
use zebra_chain::{
    serialization::ZcashSerialize,
    transaction::{Transaction, TransparentOutput},
};

#[derive(Debug, Display, Error)]
#[non_exhaustive]
pub enum Error {
    /// Verification failed but tx was valid
    #[non_exhaustive]
    ErrOk,
    /// Could not to deserialize tx
    #[non_exhaustive]
    TxDeserialize,
    /// n_in is invalid for tx
    #[non_exhaustive]
    TxIndex,
    /// tx is an invalid size for it's protocol
    #[non_exhaustive]
    TxSizeMismatch,
    /// Encountered unknown error kind from zcashconsensus: {0}
    #[non_exhaustive]
    Unknown(zcashconsensus_error_t),
}

impl From<zcashconsensus_error_t> for Error {
    #[allow(non_upper_case_globals)]
    fn from(err_code: zcashconsensus_error_t) -> Error {
        match err_code {
            zcashconsensus_error_t_zcashconsensus_ERR_OK => Error::ErrOk,
            zcashconsensus_error_t_zcashconsensus_ERR_TX_DESERIALIZE => Error::TxDeserialize,
            zcashconsensus_error_t_zcashconsensus_ERR_TX_INDEX => Error::TxIndex,
            zcashconsensus_error_t_zcashconsensus_ERR_TX_SIZE_MISMATCH => Error::TxSizeMismatch,
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
    flags: u32,
    consensus_branch_id: u32,
) -> Result<(), Error> {
    let script_pub_key = script_pub_key.as_ref();
    let tx_to = tx_to.as_ref();

    let script_ptr = script_pub_key.as_ptr();
    let script_len = script_pub_key.len();
    let tx_to_ptr = tx_to.as_ptr();
    let tx_to_len = tx_to.len();
    let mut err = 0;

    println!(
        "pubkey: {:x?}, tx_to: {:?}, amount: {}, n_in: {}, flags: {}, branch_id: {}",
        script_pub_key, tx_to, amount, n_in, flags, consensus_branch_id
    );

    let ret = unsafe {
        zcashconsensus::zcashconsensus_verify_script(
            script_ptr,
            script_len as u32,
            amount,
            tx_to_ptr,
            tx_to_len as u32,
            n_in,
            flags,
            consensus_branch_id,
            &mut err,
        )
    };

    dbg!(&ret, &err);

    if ret == 1 {
        Ok(())
    } else {
        Err(Error::from(err))
    }
}

// fn branch_id(_: Arc<Transaction>) -> ConsensusBranchId {}

pub fn script_is_valid(
    transaction: Arc<Transaction>,
    flags: u32,
    previous_outputs: &[TransparentOutput],
) -> bool {
    dbg!(&transaction);
    let tx_to = transaction.zcash_serialize_to_vec().unwrap();
    let n_in = transaction.inputs().count() - 1;
    // let branch_id = branch_id(transaction);
    let branch_id = 0x2bb40e60;

    for to_spend in previous_outputs {
        let TransparentOutput { value, pk_script } = to_spend;

        verify_script(
            &pk_script.zcash_serialize_to_vec().unwrap(),
            (*value).into(),
            &tx_to,
            n_in as _,
            flags,
            branch_id,
        )
        .unwrap();
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;
    use std::convert::TryInto;
    use std::sync::Arc;
    use zebra_chain::{serialization::ZcashDeserializeInto, types::Script};

    lazy_static::lazy_static! {
        pub static ref SCRIPT_PUBKEY: Vec<u8> = <Vec<u8>>::from_hex("76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac").unwrap();
        pub static ref SCRIPT_TX: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000").expect("Block bytes are in valid hex representation");
    }

    // #[test]
    // fn it_works() -> Result<()> {
    //     let blocks = TEST_BLOCKS
    //         .iter()
    //         .map(|bytes| bytes.zcash_deserialize_into())
    //         .collect::<Result<Vec<Arc<Block>>, _>>()?;

    //     for block in blocks {
    //         assert!(script_is_valid(&block));
    //     }

    //     Ok(())
    // }

    #[test]
    fn verify_valid_script() {
        let coin = i64::pow(10, 8);
        let script_pub_key = &*SCRIPT_PUBKEY;
        let amount = 212 * coin;
        let tx_to = &*SCRIPT_TX;
        let n_in = 0;
        let flags = 1;
        let branch_id = 0x2bb40e60;

        verify_script(script_pub_key, amount, tx_to, n_in, flags, branch_id).unwrap();
    }

    #[test]
    fn verify_valid_script_parsed() {
        let transaction = SCRIPT_TX
            .zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()
            .unwrap();
        let coin = u64::pow(10, 8);
        let amount = 212 * coin;
        let output = TransparentOutput {
            value: amount.try_into().unwrap(),
            pk_script: Script(SCRIPT_PUBKEY.clone()),
        };
        let flags = 1;

        script_is_valid(transaction, flags, &[output]);
    }

    #[test]
    #[ignore]
    fn dont_verify_invalid_script() {
        let coin = i64::pow(10, 8);
        let script_pub_key = &*SCRIPT_PUBKEY;
        let amount = 212 * coin;
        let tx_to = &*SCRIPT_TX;
        let n_in = 0;
        let flags = 1;
        let branch_id = 0x2bb40e61;

        verify_script(script_pub_key, amount, tx_to, n_in, flags, branch_id).unwrap_err();
    }
}
