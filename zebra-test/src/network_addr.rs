//! Contains test vectors for network protocol address messages:
//! * addr (v1): [addr Bitcoin Reference](https://developer.bitcoin.org/reference/p2p_networking.html#addr)
//! * addrv2: [ZIP-155](https://zips.z.cash/zip-0155#specification)
//!
//! These formats are deserialized into the
//! `zebra_network::protocol::external::Message::Addr` variant.

use hex::FromHex;
use lazy_static::lazy_static;

lazy_static! {
    /// Array of `addr` (v1) test vectors containing IP addresses.
    ///
    /// These test vectors can be read by [`zebra_network::protocol::external::Codec::read_addr`].
    /// They should produce successful results containing IP addresses.
    // From https://github.com/zingolabs/zcash/blob/9ee66e423a3fbf4829ffeec354e82f4fbceff864/src/test/netbase_tests.cpp#L397
    pub static ref ADDR_V1_IP_VECTORS: Vec<Vec<u8>> = vec![
        // stream_addrv1_hex
        <Vec<u8>>::from_hex(
            concat!(
                "02", // number of entries

                "61bc6649",                         // time, Fri Jan  9 02:54:25 UTC 2009
                "0000000000000000",                 // service flags, NODE_NONE
                "00000000000000000000000000000001", // address, fixed 16 bytes (IPv4-mapped IPv6), ::1
                "0000",                             // port, 0

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "0100000000000000",                 // service flags, NODE_NETWORK
                "00000000000000000000000000000001", // address, fixed 16 bytes (IPv4-mapped IPv6), ::1
                "00f1",                             // port, 241
            )
        ).expect("Message bytes are in valid hex representation"),

        // stream_torv3_incompatibly_serialized_to_v1
        <Vec<u8>>::from_hex(
            concat!(
                "01", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "0100000000000000",                 // service flags, NODE_NETWORK
                "00000000000000000000000000000000", // address, fixed 16 bytes (IPv4-mapped IPv6), ::
                "235a",                             // port, 9050
            )
        ).expect("Message bytes are in valid hex representation"),

        // Extra test cases:
        //
        // all services flags set
        <Vec<u8>>::from_hex(
            concat!(
                "01", // number of entries

                "61bc6649",                         // time, Fri Jan  9 02:54:25 UTC 2009
                "ffffffffffffffff",                 // service flags, all set
                "00000000000000000000000000000001", // address, fixed 16 bytes (IPv4-mapped IPv6), ::1
                "0000",                             // port, 0
            )
        ).expect("Message bytes are in valid hex representation"),

        // IPv4
        <Vec<u8>>::from_hex(
            concat!(
                "01", // number of entries

                "61bc6649",                         // time, Fri Jan  9 02:54:25 UTC 2009
                "0100000000000000",                 // service flags, NODE_NETWORK
                                                    // address, fixed 16 bytes (IPv4-mapped IPv6),
                "00000000000000000000ffff",         // IPv4-mapped IPv6 prefix, ::ffff...
                "7f000001",                         // IPv4, 127.0.0.1
                "0000",                             // port, 0
            )
        ).expect("Message bytes are in valid hex representation"),

    ];

    /// Array of empty or unsupported `addr` (v1) test vectors.
    ///
    /// These test vectors can be read by [`zebra_network::protocol::external::Codec::read_addr`].
    /// They should produce successful but empty results.
    pub static ref ADDR_V1_EMPTY_VECTORS: Vec<Vec<u8>> = vec![
        // Empty list
        <Vec<u8>>::from_hex(
            "00" // number of entries
        ).expect("Message bytes are in valid hex representation"),
    ];

    /// Array of ZIP-155 test vectors containing IP addresses.
    ///
    /// Some test vectors also contain some unsupported addresses.
    ///
    /// These test vectors can be read by [`zebra_network::protocol::external::Codec::read_addrv2`],
    /// They should produce successful results containing IP addresses.
    // From https://github.com/zingolabs/zcash/blob/9ee66e423a3fbf4829ffeec354e82f4fbceff864/src/test/netbase_tests.cpp#L421
    pub static ref ADDR_V2_IP_VECTORS: Vec<Vec<u8>> = vec![
        // stream_addrv2_hex
        <Vec<u8>>::from_hex(
            concat!(
                "03", // number of entries

                "61bc6649",                         // time, Fri Jan  9 02:54:25 UTC 2009
                "00",                               // service flags, COMPACTSIZE(NODE_NONE)
                "02",                               // network id, IPv6
                "10",                               // address length, COMPACTSIZE(16)
                "00000000000000000000000000000001", // address, ::1
                "0000",                             // port, 0

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "02",                               // network id, IPv6
                "10",                               // address length, COMPACTSIZE(16)
                "00000000000000000000000000000001", // address, ::1
                "00f1",                             // port, 241

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "04",                               // network id, TorV3
                "20",                               // address length, COMPACTSIZE(32)
                "53cd5648488c4707914182655b7664034e09e66f7e8cbf1084e654eb56c5bd88",
                                                    // address, (32 byte Tor v3 onion service public key)
                "235a",                             // port, 9050
            )
        ).expect("Message bytes are in valid hex representation"),

        // Extra test cases:
        //
        // IPv4
        <Vec<u8>>::from_hex(
            concat!(
                "02", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "01",                               // network id, IPv4
                "04",                               // address length, COMPACTSIZE(4)
                "7f000001",                         // address, 127.0.0.1
                "0001",                             // port, 1

                // check that variable-length encoding works
                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "02",                               // network id, IPv6
                "10",                               // address length, COMPACTSIZE(16)
                "00000000000000000000000000000001", // address, ::1
                "00f1",                             // port, 241
            )
        ).expect("Message bytes are in valid hex representation"),

        // all services flags set
        <Vec<u8>>::from_hex(
            concat!(
                "01", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "ffffffffffffffffff",               // service flags, COMPACTSIZE(all flags set)
                "02",                               // network id, IPv6
                "10",                               // address length, COMPACTSIZE(16)
                "00000000000000000000000000000001", // address, ::1
                "0000",                             // port, 0
            )
        ).expect("Message bytes are in valid hex representation"),

        // Unknown Network ID: address within typical size range
        <Vec<u8>>::from_hex(
            concat!(
                "02", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "fb",                               // network id, (unknown)
                "08",                               // address length, COMPACTSIZE(8)
                "0000000000000000",                 // address, (8 zero bytes)
                "0001",                             // port, 1

                // check that variable-length encoding works
                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "02",                               // network id, IPv6
                "10",                               // address length, COMPACTSIZE(16)
                "00000000000000000000000000000001", // address, ::1
                "00f1",                             // port, 241
            )
        ).expect("Message bytes are in valid hex representation"),

        // Unknown Network ID: zero-sized address
        <Vec<u8>>::from_hex(
            concat!(
                "02", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "fc",                               // network id, (unknown)
                "00",                               // address length, COMPACTSIZE(0)
                "",                                 // address, (no bytes)
                "0001",                             // port, 1

                // check that variable-length encoding works
                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "02",                               // network id, IPv6
                "10",                               // address length, COMPACTSIZE(16)
                "00000000000000000000000000000001", // address, ::1
                "00f1",                             // port, 241
            )
        ).expect("Message bytes are in valid hex representation"),

        // Unknown Network ID: maximum-sized address
        <Vec<u8>>::from_hex(
            format!("{}{}{}",
                    concat!(
                        "02", // number of entries

                        "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                        "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                        "fd",                               // network id, (unknown)
                        "fd0002",                           // address length, COMPACTSIZE(512)
                    ),
                    "00".repeat(512),                       // address, (512 zero bytes)
                    concat!(
                        "0001",                             // port, 1

                        // check that variable-length encoding works
                        "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                        "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                        "02",                               // network id, IPv6
                        "10",                               // address length, COMPACTSIZE(16)
                        "00000000000000000000000000000001", // address, ::1
                        "00f1",                             // port, 241
                    )
            )
        ).expect("Message bytes are in valid hex representation"),
    ];

    /// Array of empty or unsupported ZIP-155 test vectors.
    ///
    /// These test vectors can be read by [`zebra_network::protocol::external::Codec::read_addrv2`].
    /// They should produce successful but empty results.
    // From https://github.com/zingolabs/zcash/blob/9ee66e423a3fbf4829ffeec354e82f4fbceff864/src/test/netbase_tests.cpp#L421
    pub static ref ADDR_V2_EMPTY_VECTORS: Vec<Vec<u8>> = vec![
        // torv3_hex
        <Vec<u8>>::from_hex(
            concat!(
                "01", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "04",                               // network id, TorV3
                "20",                               // address length, COMPACTSIZE(32)
                "53cd5648488c4707914182655b7664034e09e66f7e8cbf1084e654eb56c5bd88",
                                                    // address, (32 byte Tor v3 onion service public key)
                "235a",                             // port, 9050
            )
        ).expect("Message bytes are in valid hex representation"),

        // Extra test cases:
        //
        // Empty list
        <Vec<u8>>::from_hex(
            "00" // number of entries
        ).expect("Message bytes are in valid hex representation"),
    ];

    /// Array of invalid ZIP-155 test vectors.
    ///
    /// These test vectors can be read by [`zebra_network::protocol::external::Codec::read_addrv2`].
    /// They should fail deserialization.
    pub static ref ADDR_V2_INVALID_VECTORS: Vec<Vec<u8>> = vec![
        // Invalid address size: too large, but under CompactSizeMessage limit
        <Vec<u8>>::from_hex(
            format!("{}{}{}",
                    concat!(
                        "02", // number of entries

                        "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                        "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                        "fe",                               // network id, (unknown)
                        "fd0102",                           // invalid address length, COMPACTSIZE(513)
                    ),
                    "00".repeat(513),                       // address, (513 zero bytes)
                    concat!(
                        "0001",                             // port, 1

                        // check that the entire message is ignored
                        "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                        "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                        "02",                               // network id, IPv6
                        "10",                               // address length, COMPACTSIZE(16)
                        "00000000000000000000000000000001", // address, ::1
                        "00f1",                             // port, 241
                    )
            )
        ).expect("Message bytes are in valid hex representation"),

        // Invalid address size: too large, over CompactSizeMessage limit
        <Vec<u8>>::from_hex(
            concat!(
                "01", // number of entries

                "79627683",                         // time, Tue Nov 22 11:22:33 UTC 2039
                "01",                               // service flags, COMPACTSIZE(NODE_NETWORK)
                "ff",                               // network id, (unknown)
                "feffffff7f",                       // invalid address length, COMPACTSIZE(2^31 - 1)
                // no address, generated bytes wouldn't fit in memory
            ),
        ).expect("Message bytes are in valid hex representation"),
    ];
}
