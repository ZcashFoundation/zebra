//! Randomised property tests for MetaAddr.

use super::check;

use crate::meta_addr::{arbitrary::MAX_ADDR_CHANGE, MetaAddr, MetaAddrChange, PeerAddrState::*};

use proptest::prelude::*;

use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};

proptest! {
    /// Make sure that the sanitize function reduces time and state metadata
    /// leaks.
    #[test]
    fn sanitize_avoids_leaks(addr in MetaAddr::arbitrary()) {
        zebra_test::init();

        if let Some(sanitized) = addr.sanitize() {
            check::sanitize_avoids_leaks(&addr, &sanitized);
        }
    }

    /// Test round-trip serialization for gossiped MetaAddrs
    #[test]
    fn gossiped_roundtrip(
        gossiped_addr in MetaAddr::gossiped_strategy()
    ) {
        zebra_test::init();

        // We require sanitization before serialization
        let gossiped_addr = gossiped_addr.sanitize();
        prop_assume!(gossiped_addr.is_some());
        let gossiped_addr = gossiped_addr.unwrap();

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

    /// Test round-trip serialization for all MetaAddr variants after sanitization
    #[test]
    fn sanitized_roundtrip(
        addr in any::<MetaAddr>()
    ) {
        zebra_test::init();

        // We require sanitization before serialization,
        // but we also need the original address for this test
        let sanitized_addr = addr.sanitize();
        prop_assume!(sanitized_addr.is_some());
        let sanitized_addr = sanitized_addr.unwrap();

        // Make sure sanitization avoids leaks on this address, to avoid spurious errors
        check::sanitize_avoids_leaks(&addr, &sanitized_addr);

        // Check that sanitization doesn't make Zebra's serialization fail
        let addr_bytes = sanitized_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes.is_ok(),
            "unexpected serialization error: {:?}, addr: {:?}",
            addr_bytes,
            sanitized_addr
        );
        let addr_bytes = addr_bytes.unwrap();

        // Assume other implementations deserialize like Zebra
        let deserialized_addr = MetaAddr::zcash_deserialize(addr_bytes.as_slice());
        prop_assert!(
            deserialized_addr.is_ok(),
            "unexpected deserialization error: {:?}, addr: {:?}, bytes: {:?}",
            deserialized_addr,
            sanitized_addr,
            hex::encode(addr_bytes),
        );
        let deserialized_addr = deserialized_addr.unwrap();

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
        let addr_bytes2 = deserialized_addr.zcash_serialize_to_vec();
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
            "unexpected round-trip bytes mismatch: original addr: {:?}, bytes: {:?}, deserialized addr: {:?}, bytes: {:?}",
            sanitized_addr,
            hex::encode(&addr_bytes),
            deserialized_addr,
            hex::encode(&addr_bytes2),
        );

    }

    /// Make sure that `[MetaAddrChange]`s:
    /// - do not modify the last seen time, unless it was None, and
    /// - only modify the services after a response or failure.
    #[test]
    fn preserve_initial_untrusted_values((mut addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)) {
        zebra_test::init();

        for change in changes {
            if let Some(changed_addr) = change.apply_to_meta_addr(addr) {
                // untrusted last seen times:
                // check that we replace None with Some, but leave Some unchanged
                if addr.untrusted_last_seen.is_some() {
                    prop_assert_eq!(changed_addr.untrusted_last_seen, addr.untrusted_last_seen);
                } else {
                    prop_assert_eq!(changed_addr.untrusted_last_seen, change.untrusted_last_seen());
                }

                // services:
                // check that we only change if there was a handshake
                if changed_addr.last_connection_state.is_never_attempted()
                    || changed_addr.last_connection_state == AttemptPending
                    || change.untrusted_services().is_none() {
                    prop_assert_eq!(changed_addr.services, addr.services);
                }

                addr = changed_addr;
            }
        }
    }
}
