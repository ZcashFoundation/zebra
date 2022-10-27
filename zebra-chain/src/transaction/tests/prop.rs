//! Randomised property tests for transactions.

use proptest::prelude::*;

use std::io::Cursor;

use zebra_test::prelude::*;

use hex::{FromHex, ToHex};

use super::super::*;

use crate::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::arbitrary::MAX_ARBITRARY_ITEMS,
    LedgerState,
};

proptest! {
    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let _init_guard = zebra_test::init();

        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2 = data.zcash_deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![&tx, &tx2];

        let data2 = tx2
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    #[test]
    fn transaction_hash_struct_display_roundtrip(hash in any::<Hash>()) {
        let _init_guard = zebra_test::init();

        let display = format!("{hash}");
        let parsed = display.parse::<Hash>().expect("hash should parse");
        prop_assert_eq!(hash, parsed);
    }

    #[test]
    fn transaction_hash_string_parse_roundtrip(hash in any::<String>()) {
        let _init_guard = zebra_test::init();

        if let Ok(parsed) = hash.parse::<Hash>() {
            let display = format!("{parsed}");
            prop_assert_eq!(hash, display);
        }
    }

    #[test]
    fn transaction_hash_hex_roundtrip(hash in any::<Hash>()) {
        let _init_guard = zebra_test::init();

        let hex_hash: String = hash.encode_hex();
        let new_hash = Hash::from_hex(hex_hash).expect("hex hash should parse");
        prop_assert_eq!(hash, new_hash);
    }

    #[test]
    fn transaction_auth_digest_struct_display_roundtrip(auth_digest in any::<AuthDigest>()) {
        let _init_guard = zebra_test::init();

        let display = format!("{auth_digest}");
        let parsed = display.parse::<AuthDigest>().expect("auth digest should parse");
        prop_assert_eq!(auth_digest, parsed);
    }

    #[test]
    fn transaction_auth_digest_string_parse_roundtrip(auth_digest in any::<String>()) {
        let _init_guard = zebra_test::init();

        if let Ok(parsed) = auth_digest.parse::<AuthDigest>() {
            let display = format!("{parsed}");
            prop_assert_eq!(auth_digest, display);
        }
    }

    #[test]
    fn transaction_wtx_id_struct_display_roundtrip(wtx_id in any::<WtxId>()) {
        let _init_guard = zebra_test::init();

        let display = format!("{wtx_id}");
        let parsed = display.parse::<WtxId>().expect("wide transaction ID should parse");
        prop_assert_eq!(wtx_id, parsed);
    }

    #[test]
    fn transaction_wtx_id_string_parse_roundtrip(wtx_id in any::<String>()) {
        let _init_guard = zebra_test::init();

        if let Ok(parsed) = wtx_id.parse::<WtxId>() {
            let display = format!("{parsed}");
            prop_assert_eq!(wtx_id, display);
        }
    }

    #[test]
    fn locktime_roundtrip(locktime in any::<LockTime>()) {
        let _init_guard = zebra_test::init();

        let mut bytes = Cursor::new(Vec::new());
        locktime.zcash_serialize(&mut bytes)?;

        bytes.set_position(0);
        let other_locktime = LockTime::zcash_deserialize(&mut bytes)?;

        prop_assert_eq![locktime, other_locktime];
    }
}

/// Make sure a transaction version override generates transactions with the specified
/// transaction versions.
#[test]
fn arbitrary_transaction_version_strategy() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Update with new transaction versions as needed
    let strategy = (1..5u32)
        .prop_flat_map(|transaction_version| {
            LedgerState::coinbase_strategy(None, transaction_version, false)
        })
        .prop_flat_map(|ledger_state| Transaction::vec_strategy(ledger_state, MAX_ARBITRARY_ITEMS));

    proptest!(|(transactions in strategy)| {
        let mut version = None;
        for t in transactions {
            if version.is_none() {
                version = Some(t.version());
            } else {
                prop_assert_eq!(Some(t.version()), version);
            }
        }
    });

    Ok(())
}

/// Make sure a transaction valid network upgrade strategy generates transactions
/// with valid network upgrades.
#[test]
fn transaction_valid_network_upgrade_strategy() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Update with new transaction versions as needed
    let strategy = LedgerState::coinbase_strategy(None, 5, true).prop_flat_map(|ledger_state| {
        (
            Just(ledger_state.network),
            Block::arbitrary_with(ledger_state),
        )
    });

    proptest!(|((network, block) in strategy)| {
        block.check_transaction_network_upgrade_consistency(network)?;
    });

    Ok(())
}
