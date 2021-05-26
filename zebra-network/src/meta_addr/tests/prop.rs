//! Randomised property tests for MetaAddr.

use super::{super::MetaAddr, check};

use proptest::prelude::*;

use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};

proptest! {
    /// Make sure that the sanitize function reduces time and state metadata
    /// leaks.
    #[test]
    fn sanitized_fields(entry in MetaAddr::arbitrary()) {
        zebra_test::init();

        check::sanitize_avoids_leaks(&entry);
    }

    /// Test round-trip serialization for gossiped MetaAddrs
    #[test]
    fn gossiped_roundtrip(
        mut gossiped_addr in MetaAddr::gossiped_strategy()
    ) {
        zebra_test::init();

        // Zebra's deserialization sanitizes `services` to known flags
        gossiped_addr.services &= PeerServices::all();

        // Check that malicious peers can't make Zebra's serialization fail
        let addr_bytes = gossiped_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes.is_ok(),
            "unexpected serialization error: {:?}, addr: {:?}",
            addr_bytes,
            gossiped_addr
        );
        let addr_bytes = addr_bytes.unwrap();

        // Assume other implementations deserialize like Zebra
        let deserialized_addr = MetaAddr::zcash_deserialize(addr_bytes.as_slice());
        prop_assert!(
            deserialized_addr.is_ok(),
            "unexpected deserialization error: {:?}, addr: {:?}, bytes: {:?}",
            deserialized_addr,
            gossiped_addr,
            hex::encode(addr_bytes),
        );
        let deserialized_addr = deserialized_addr.unwrap();

        // Check that the addrs are equal
        prop_assert_eq!(
            gossiped_addr,
            deserialized_addr,
            "unexpected round-trip mismatch with bytes: {:?}",
            hex::encode(addr_bytes),
        );

        // Now check that the re-serialized bytes are equal
        // (`impl PartialEq for MetaAddr` might not match serialization equality)
        let addr_bytes2 = deserialized_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes2.is_ok(),
            "unexpected serialization error after round-trip: {:?}, original addr: {:?}, bytes: {:?}, deserialized addr: {:?}",
            addr_bytes2,
            gossiped_addr,
            hex::encode(addr_bytes),
            deserialized_addr,
        );
        let addr_bytes2 = addr_bytes2.unwrap();

        prop_assert_eq!(
            &addr_bytes,
            &addr_bytes2,
            "unexpected round-trip bytes mismatch: original addr: {:?}, bytes: {:?}, deserialized addr: {:?}, bytes: {:?}",
            gossiped_addr,
            hex::encode(&addr_bytes),
            deserialized_addr,
            hex::encode(&addr_bytes2),
        );

    }
}
