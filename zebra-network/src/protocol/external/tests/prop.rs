//! Randomised property tests for Zebra's Zcash network protocol types.

use bytes::BytesMut;
use proptest::{collection::vec, prelude::*};
use tokio_util::codec::{Decoder, Encoder};

use zebra_chain::{
    parameters::Network::*,
    serialization::{
        SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
        MAX_PROTOCOL_MESSAGE_LEN,
    },
};

use crate::{
    meta_addr::{tests::check, MetaAddr},
    protocol::external::{
        addr::{AddrV1, AddrV2},
        Codec, InventoryHash, Message,
    },
};

/// Maximum number of random input bytes to try to deserialize an [`InventoryHash`] from.
///
/// This is two bytes larger than the maximum [`InventoryHash`] size.
const MAX_INVENTORY_HASH_BYTES: usize = 70;

proptest! {
    /// Test if [`InventoryHash`] is not changed after serializing and deserializing it.
    #[test]
    fn inventory_hash_roundtrip(inventory_hash in any::<InventoryHash>()) {
        let mut bytes = Vec::new();

        let serialization_result = inventory_hash.zcash_serialize(&mut bytes);

        prop_assert!(serialization_result.is_ok());
        prop_assert!(bytes.len() < MAX_INVENTORY_HASH_BYTES);

        let deserialized: Result<InventoryHash, _> = bytes.zcash_deserialize_into();

        prop_assert!(deserialized.is_ok());
        prop_assert_eq!(deserialized.unwrap(), inventory_hash);
    }

    /// Test attempting to deserialize an [`InventoryHash`] from random bytes.
    #[test]
    fn inventory_hash_from_random_bytes(input in vec(any::<u8>(), 0..MAX_INVENTORY_HASH_BYTES)) {
        let deserialized: Result<InventoryHash, _> = input.zcash_deserialize_into();

        if input.len() >= 4 && (input[1..4] != [0u8; 3] || input[0] > 5 || input[0] == 4) {
            // Invalid inventory code
            prop_assert!(matches!(
                deserialized,
                Err(SerializationError::Parse("invalid inventory code"))
            ));
        } else if input.len() < 36 {
            // Not enough bytes for any inventory hash
            prop_assert!(deserialized.is_err());
            prop_assert_eq!(
                deserialized.unwrap_err().to_string(),
                "io error: failed to fill whole buffer"
            );
        } else if input[0] == 5 && input.len() < 68 {
            // Not enough bytes for a WTX inventory hash
            prop_assert!(deserialized.is_err());
            prop_assert_eq!(
                deserialized.unwrap_err().to_string(),
                "io error: failed to fill whole buffer"
            );
        } else {
            // Deserialization should have succeeded
            prop_assert!(deserialized.is_ok());

            // Reserialize inventory hash
            let mut bytes = Vec::new();
            let serialization_result = deserialized.unwrap().zcash_serialize(&mut bytes);

            prop_assert!(serialization_result.is_ok());

            // Check that the reserialization produces the same bytes as the input
            prop_assert!(bytes.len() <= input.len());
            prop_assert_eq!(&bytes, &input[..bytes.len()]);
        }
    }

    /// Test if a [`Message::{Inv, GetData}`] is not changed after encoding and decoding it.
    // TODO: Update this test to cover all `Message` variants.
    #[test]
    fn inv_and_getdata_message_roundtrip(
        message in prop_oneof!(Message::inv_strategy(), Message::get_data_strategy()),
    ) {
        let mut codec = Codec::builder().finish();
        let mut bytes = BytesMut::with_capacity(MAX_PROTOCOL_MESSAGE_LEN);

        let encoding_result = codec.encode(message.clone(), &mut bytes);

        prop_assert!(encoding_result.is_ok());

        let decoded: Result<Option<Message>, _> = codec.decode(&mut bytes);

        prop_assert!(decoded.is_ok());
        prop_assert_eq!(decoded.unwrap(), Some(message));
    }

    /// Test round-trip AddrV1 serialization for all MetaAddr variants after sanitization
    #[test]
    fn addr_v1_sanitized_roundtrip(addr in any::<MetaAddr>()) {
        let _init_guard = zebra_test::init();

        // We require sanitization before serialization,
        // but we also need the original address for this test
        let sanitized_addr = addr.sanitize(&Mainnet);
        prop_assume!(sanitized_addr.is_some());
        let sanitized_addr = sanitized_addr.unwrap();

        // Make sure sanitization avoids leaks on this address, to avoid spurious errors
        check::sanitize_avoids_leaks(&addr, &sanitized_addr);

        // Check that sanitization doesn't make Zebra's serialization fail.
        //
        // If this is a gossiped or DNS seeder address,
        // we're also checking that malicious peers can't make Zebra's serialization fail.
        let addr_bytes = AddrV1::from(sanitized_addr).zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes.is_ok(),
            "unexpected serialization error: {:?}, addr: {:?}",
            addr_bytes,
            sanitized_addr
        );
        let addr_bytes = addr_bytes.unwrap();

        // Assume other implementations deserialize like Zebra
        let deserialized_addr = AddrV1::zcash_deserialize(addr_bytes.as_slice());
        prop_assert!(
            deserialized_addr.is_ok(),
            "unexpected deserialization error: {:?}, addr: {:?}, bytes: {:?}",
            deserialized_addr,
            sanitized_addr,
            hex::encode(addr_bytes),
        );
        let deserialized_addr: MetaAddr = deserialized_addr.unwrap().into();

        // Check that the addrs are equal
        prop_assert_eq!(
            sanitized_addr,
            deserialized_addr,
            "unexpected round-trip mismatch with bytes: {:?}",
            hex::encode(addr_bytes),
        );

        // Check that serialization hasn't de-sanitized anything
        check::sanitize_avoids_leaks(&addr, &deserialized_addr);

        // Now check that the re-serialized bytes are equal
        // (`impl PartialEq for MetaAddr` might not match serialization equality)
        let addr_bytes2 = AddrV1::from(deserialized_addr).zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes2.is_ok(),
            "unexpected serialization error after round-trip: {:?}, original addr: {:?}, bytes: {:?}, deserialized addr: {:?}",
            addr_bytes2,
            sanitized_addr,
            hex::encode(addr_bytes),
            deserialized_addr,
        );
        let addr_bytes2 = addr_bytes2.unwrap();

        prop_assert_eq!(
            &addr_bytes,
            &addr_bytes2,
            "unexpected double-serialization round-trip mismatch with original addr: {:?}, bytes: {:?}, deserialized addr: {:?}, bytes: {:?}",
            sanitized_addr,
            hex::encode(&addr_bytes),
            deserialized_addr,
            hex::encode(&addr_bytes2),
        );
    }

    /// Test round-trip AddrV2 serialization for all MetaAddr variants after sanitization
    #[test]
    fn addr_v2_sanitized_roundtrip(addr in any::<MetaAddr>()) {
        let _init_guard = zebra_test::init();

        // We require sanitization before serialization,
        // but we also need the original address for this test
        let sanitized_addr = addr.sanitize(&Mainnet);
        prop_assume!(sanitized_addr.is_some());
        let sanitized_addr = sanitized_addr.unwrap();

        // Make sure sanitization avoids leaks on this address, to avoid spurious errors
        check::sanitize_avoids_leaks(&addr, &sanitized_addr);

        // Check that sanitization doesn't make Zebra's serialization fail.
        //
        // If this is a gossiped or DNS seeder address,
        // we're also checking that malicious peers can't make Zebra's serialization fail.
        let addr_bytes = AddrV2::from(sanitized_addr).zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes.is_ok(),
            "unexpected serialization error: {:?}, addr: {:?}",
            addr_bytes,
            sanitized_addr
        );
        let addr_bytes = addr_bytes.unwrap();

        // Assume other implementations deserialize like Zebra
        let deserialized_addr = AddrV2::zcash_deserialize(addr_bytes.as_slice());
        prop_assert!(
            deserialized_addr.is_ok(),
            "unexpected deserialization error: {:?}, addr: {:?}, bytes: {:?}",
            deserialized_addr,
            sanitized_addr,
            hex::encode(addr_bytes),
        );
        let deserialized_addr: AddrV2 = deserialized_addr.unwrap();
        let deserialized_addr: MetaAddr = deserialized_addr.try_into().expect("arbitrary MetaAddrs are IPv4 or IPv6");

        // Check that the addrs are equal
        prop_assert_eq!(
            sanitized_addr,
            deserialized_addr,
            "unexpected round-trip mismatch with bytes: {:?}",
            hex::encode(addr_bytes),
        );

        // Check that serialization hasn't de-sanitized anything
        check::sanitize_avoids_leaks(&addr, &deserialized_addr);

        // Now check that the re-serialized bytes are equal
        // (`impl PartialEq for MetaAddr` might not match serialization equality)
        let addr_bytes2 = AddrV2::from(deserialized_addr).zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes2.is_ok(),
            "unexpected serialization error after round-trip: {:?}, original addr: {:?}, bytes: {:?}, deserialized addr: {:?}",
            addr_bytes2,
            sanitized_addr,
            hex::encode(addr_bytes),
            deserialized_addr,
        );
        let addr_bytes2 = addr_bytes2.unwrap();

        prop_assert_eq!(
            &addr_bytes,
            &addr_bytes2,
            "unexpected double-serialization round-trip mismatch with original addr: {:?}, bytes: {:?}, deserialized addr: {:?}, bytes: {:?}",
            sanitized_addr,
            hex::encode(&addr_bytes),
            deserialized_addr,
            hex::encode(&addr_bytes2),
        );
    }
}
