#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_script")]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

pub use ffi::zcashconsensus_error_t;

pub fn verify_script(
    script_pub_key: &[u8],
    amount: i64,
    tx_to: &[u8],
    nIn: u32,
    flags: u32,
    consensus_branch_id: u32,
) -> Result<(), zcashconsensus_error_t> {
    let script_ptr = script_pub_key.as_ptr();
    let script_len = script_pub_key.len();
    let tx_to_ptr = tx_to.as_ptr();
    let tx_to_len = tx_to.len();
    let mut err = 0;

    let ret = unsafe {
        ffi::zcashconsensus_verify_script(
            script_ptr,
            script_len as u32,
            amount,
            tx_to_ptr,
            tx_to_len as u32,
            nIn,
            flags,
            consensus_branch_id,
            &mut err,
        )
    };

    if ret == 1 {
        Ok(())
    } else {
        Err(err)
    }
}

#[cfg(test)]
mod tests {
    use hex::FromHex;

    lazy_static::lazy_static! {
        pub static ref SCRIPT_PUBKEY: Vec<u8> = <Vec<u8>>::from_hex("76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac").unwrap();
        pub static ref SCRIPT_TX: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000").expect("Block bytes are in valid hex representation");
    }


    #[test]
    fn it_works() {
        let coin = i64::pow(10, 8);
        let script_pub_key = &*SCRIPT_PUBKEY;
        let amount = 212 * coin;
        let tx_to = &*SCRIPT_TX;
        let nIn = 0;
        let flags = 1;
        let branch_id = 0x2bb40e60;

        super::verify_script(script_pub_key, amount, tx_to, nIn, flags, branch_id).unwrap();
    }
}
