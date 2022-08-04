//! Fixed test vectors for external protocol messages.

use std::{convert::TryInto, io::Write};

use byteorder::{LittleEndian, WriteBytesExt};

use chrono::{DateTime, Utc};
use zebra_chain::serialization::ZcashDeserializeInto;

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{types::PeerServices, Codec, InventoryHash, Message},
};

/// Test if deserializing [`InventoryHash::Wtx`] does not produce an error.
#[test]
fn parses_msg_wtx_inventory_type() {
    let _init_guard = zebra_test::init();

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
///
/// Also checks some of the deserialized address values.
#[test]
fn parses_msg_addr_v1_ip() {
    let _init_guard = zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v1_bytes) in zebra_test::network_addr::ADDR_V1_IP_VECTORS
        .iter()
        .enumerate()
    {
        let deserialized: Message = codec
            .read_addr(&mut addr_v1_bytes.as_slice())
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

            // Check all the fields in the first test case
            if case_idx == 0 {
                assert_eq!(
                    addrs,
                    vec![
                        MetaAddr::new_gossiped_meta_addr(
                            "[::1]:0".parse().unwrap(),
                            PeerServices::empty(),
                            DateTime::parse_from_rfc3339("2009-01-09T02:54:25+00:00")
                                .unwrap()
                                .with_timezone(&Utc)
                                .try_into()
                                .unwrap(),
                        ),
                        MetaAddr::new_gossiped_meta_addr(
                            "[::1]:241".parse().unwrap(),
                            PeerServices::NODE_NETWORK,
                            DateTime::parse_from_rfc3339("2039-11-22T11:22:33+00:00")
                                .unwrap()
                                .with_timezone(&Utc)
                                .try_into()
                                .unwrap(),
                        ),
                    ],
                );
            }
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
    let _init_guard = zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v1_bytes) in zebra_test::network_addr::ADDR_V1_EMPTY_VECTORS
        .iter()
        .enumerate()
    {
        let deserialized: Message = codec
            .read_addr(&mut addr_v1_bytes.as_slice())
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
///
/// Also checks some of the deserialized address values.
#[test]
fn parses_msg_addr_v2_ip() {
    let _init_guard = zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v2_bytes) in zebra_test::network_addr::ADDR_V2_IP_VECTORS
        .iter()
        .enumerate()
    {
        let deserialized: Message = codec
            .read_addrv2(&mut addr_v2_bytes.as_slice())
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

            // Check all the fields in the IPv4 and IPv6 test cases
            if case_idx == 0 {
                assert_eq!(
                    addrs,
                    vec![
                        MetaAddr::new_gossiped_meta_addr(
                            "[::1]:0".parse().unwrap(),
                            PeerServices::empty(),
                            DateTime::parse_from_rfc3339("2009-01-09T02:54:25+00:00")
                                .unwrap()
                                .with_timezone(&Utc)
                                .try_into()
                                .unwrap(),
                        ),
                        MetaAddr::new_gossiped_meta_addr(
                            "[::1]:241".parse().unwrap(),
                            PeerServices::NODE_NETWORK,
                            DateTime::parse_from_rfc3339("2039-11-22T11:22:33+00:00")
                                .unwrap()
                                .with_timezone(&Utc)
                                .try_into()
                                .unwrap(),
                        ),
                        // torv3 is unsupported, so it's not in the parsed list
                    ],
                );
            } else if case_idx == 1 {
                assert_eq!(
                    addrs,
                    vec![
                        MetaAddr::new_gossiped_meta_addr(
                            "127.0.0.1:1".parse().unwrap(),
                            PeerServices::NODE_NETWORK,
                            DateTime::parse_from_rfc3339("2039-11-22T11:22:33+00:00")
                                .unwrap()
                                .with_timezone(&Utc)
                                .try_into()
                                .unwrap(),
                        ),
                        MetaAddr::new_gossiped_meta_addr(
                            "[::1]:241".parse().unwrap(),
                            PeerServices::NODE_NETWORK,
                            DateTime::parse_from_rfc3339("2039-11-22T11:22:33+00:00")
                                .unwrap()
                                .with_timezone(&Utc)
                                .try_into()
                                .unwrap(),
                        ),
                    ],
                );
            }
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
    let _init_guard = zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v2_bytes) in zebra_test::network_addr::ADDR_V2_EMPTY_VECTORS
        .iter()
        .enumerate()
    {
        let deserialized: Message = codec
            .read_addrv2(&mut addr_v2_bytes.as_slice())
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
    let _init_guard = zebra_test::init();

    let codec = Codec::builder().finish();

    for (case_idx, addr_v2_bytes) in zebra_test::network_addr::ADDR_V2_INVALID_VECTORS
        .iter()
        .enumerate()
    {
        codec
            .read_addrv2(&mut addr_v2_bytes.as_slice())
            .expect_err(&format!(
                "unexpected success: deserializing invalid AddrV2 case {} should have failed",
                case_idx
            ));
    }
}
