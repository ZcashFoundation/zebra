use std::io::{self, Write};

use byteorder::{LittleEndian, WriteBytesExt};

use zebra_chain::serialization::ZcashDeserializeInto;

use crate::protocol::external::{Codec, InventoryHash, Message};

/// Test if deserializing [`InventoryHash::Wtx`] does not produce an error.
#[test]
fn parses_msg_wtx_inventory_type() {
    zebra_test::init();

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

/// Test that deserializing [`AddrV1`] into [`MetaAddr`] succeeds,
/// and produces the expected number of addresses.
#[test]
fn parses_msg_addr_v1_ip() {
    zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v1_bytes) in zebra_test::network_addr::ADDR_V1_IP_VECTORS
        .iter()
        .enumerate()
    {
        let mut addr_v1_bytes = io::Cursor::new(addr_v1_bytes);

        let deserialized: Message = codec
            .read_addr(&mut addr_v1_bytes)
            .unwrap_or_else(|_| panic!("failed to deserialize AddrV1 case {}", case_idx));

        if let Message::Addr(addrs) = deserialized {
            assert!(
                !addrs.is_empty(),
                "expected some AddrV1s in case {}: {:?}",
                case_idx,
                addrs
            );
            assert!(
                addrs.len() <= 2,
                "too many AddrV1s in case {}: {:?}",
                case_idx,
                addrs
            );
        } else {
            panic!(
                "unexpected message variant in case {}: {:?}",
                case_idx, deserialized
            );
        }
    }
}

/// Test that deserializing empty [`AddrV1`] succeeds,
/// and produces no addresses.
#[test]
fn parses_msg_addr_v1_empty() {
    zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v1_bytes) in zebra_test::network_addr::ADDR_V1_EMPTY_VECTORS
        .iter()
        .enumerate()
    {
        let mut addr_v1_bytes = io::Cursor::new(addr_v1_bytes);

        let deserialized: Message = codec
            .read_addr(&mut addr_v1_bytes)
            .unwrap_or_else(|_| panic!("failed to deserialize AddrV1 case {}", case_idx));

        if let Message::Addr(addrs) = deserialized {
            assert!(
                addrs.is_empty(),
                "expected empty AddrV1 list for case {}: {:?}",
                case_idx,
                addrs,
            );
        } else {
            panic!(
                "unexpected message variant in case {}: {:?}",
                case_idx, deserialized
            );
        }
    }
}

/// Test that deserializing [`AddrV2`] into [`MetaAddr`] succeeds,
/// and produces the expected number of addresses.
#[test]
fn parses_msg_addr_v2_ip() {
    zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v2_bytes) in zebra_test::network_addr::ADDR_V2_IP_VECTORS
        .iter()
        .enumerate()
    {
        let mut addr_v2_bytes = io::Cursor::new(addr_v2_bytes);

        let deserialized: Message = codec
            .read_addrv2(&mut addr_v2_bytes)
            .unwrap_or_else(|_| panic!("failed to deserialize AddrV2 case {}", case_idx));

        if let Message::Addr(addrs) = deserialized {
            assert!(
                !addrs.is_empty(),
                "expected some AddrV2s in case {}: {:?}",
                case_idx,
                addrs
            );
            assert!(
                addrs.len() <= 2,
                "too many AddrV2s in case {}: {:?}",
                case_idx,
                addrs
            );
        } else {
            panic!(
                "unexpected message variant in case {}: {:?}",
                case_idx, deserialized
            );
        }
    }
}

/// Test that deserializing empty [`AddrV2`] succeeds,
/// and produces no addresses.
#[test]
fn parses_msg_addr_v2_empty() {
    zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v2_bytes) in zebra_test::network_addr::ADDR_V2_EMPTY_VECTORS
        .iter()
        .enumerate()
    {
        let mut addr_v2_bytes = io::Cursor::new(addr_v2_bytes);

        let deserialized: Message = codec
            .read_addrv2(&mut addr_v2_bytes)
            .unwrap_or_else(|_| panic!("failed to deserialize AddrV2 case {}", case_idx));

        if let Message::Addr(addrs) = deserialized {
            assert!(
                addrs.is_empty(),
                "expected empty AddrV2 list for case {}: {:?}",
                case_idx,
                addrs,
            );
        } else {
            panic!(
                "unexpected message variant in case {}: {:?}",
                case_idx, deserialized
            );
        }
    }
}

/// Test that deserializing invalid [`AddrV2`] fails.
#[test]
fn parses_msg_addr_v2_invalid() {
    zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v2_bytes) in zebra_test::network_addr::ADDR_V2_INVALID_VECTORS
        .iter()
        .enumerate()
    {
        let mut addr_v2_bytes = io::Cursor::new(addr_v2_bytes);

        codec.read_addrv2(&mut addr_v2_bytes).expect_err(&format!(
            "unexpected success: deserializing invalid AddrV2 case {} should have failed",
            case_idx
        ));
    }
}
