//! Orchard shielded data (with Actions) test vectors

#![allow(missing_docs)]

use hex::FromHex;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref ORCHARD_SHIELDED_DATA: Vec<&'static [u8]> = [
        ORCHARD_SHIELDED_DATA_1_BYTES.as_ref(),
        ORCHARD_SHIELDED_DATA_3_BYTES.as_ref(),
        ORCHARD_SHIELDED_DATA_3_BYTES.as_ref(),
        ORCHARD_SHIELDED_DATA_4_BYTES.as_ref(),
    ]
    .iter()
    .cloned()
    .collect();
    pub static ref ORCHARD_SHIELDED_DATA_1_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("orchard-shielded-data-1.txt").trim())
            .expect("Orchard shielded data bytes are in valid hex representation");
    pub static ref ORCHARD_SHIELDED_DATA_2_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("orchard-shielded-data-2.txt").trim())
            .expect("Orchard shielded data bytes are in valid hex representation");
    pub static ref ORCHARD_SHIELDED_DATA_3_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("orchard-shielded-data-3.txt").trim())
            .expect("Orchard shielded data bytes are in valid hex representation");
    pub static ref ORCHARD_SHIELDED_DATA_4_BYTES: Vec<u8> =
        <Vec<u8>>::from_hex(include_str!("orchard-shielded-data-4.txt").trim())
            .expect("Orchard shielded data bytes are in valid hex representation");
}
