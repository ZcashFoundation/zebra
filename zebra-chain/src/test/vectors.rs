//! Test vectors for blockchain contructions
use hex::FromHex;

use lazy_static::lazy_static;

lazy_static! {
    pub static ref DUMMY_TX1 : Vec<u8> = <Vec<u8>>::from_hex("01000000019921d81f33e0c8b53a23d2e60643807bfe00e59fbb5f3d3e6fba20e73c2049a00000000000ffffffff0140420f00000000001976a914588cff9d4339d754758ade214b3edc69ce57b7f588ac00000000").expect("Block bytes are in valid hex representation");
    pub static ref DUMMY_INPUT1 : Vec<u8> = <Vec<u8>>::from_hex("1d322261f61dd7093b1880b735152cf0ed19beabee374046e69559c9fb8858bba0000000000ffffffff0").expect("Input bytes are in valid hex representation");
    pub static ref DUMMY_OUTPUT1 : Vec<u8> = <Vec<u8>>::from_hex("0140420f00000000001976a914588cff9d4339d754758ade214b3edc69ce57b7f588ac").expect("Output bytes are in valid hex representation");
}
