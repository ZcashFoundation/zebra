use std::io::Write;

use byteorder::{LittleEndian, WriteBytesExt};

use zebra_chain::serialization::ZcashDeserializeInto;

use super::super::InventoryHash;

/// Test if deserializing [`InventoryHash::Wtx`] does not produce an error.
#[test]
fn parses_msg_wtx_inventory_type() {
    let mut input = Vec::new();

    input
        .write_u32::<LittleEndian>(5)
        .expect("Failed to write MSG_WTX code");
    input
        .write_all(&[0u8; 64])
        .expect("Failed to write dummy inventory data");

    let deserialized: InventoryHash = input
        .zcash_deserialize_into()
        .expect("Failed to deserialize dummy `InventoryHash::Wtx`");

    assert_eq!(deserialized, InventoryHash::Wtx([0u8; 64].into()));
}
