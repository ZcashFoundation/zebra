//! Randomised property tests for transactions.

use proptest::{prelude::*, strategy::ValueTree, test_runner::TestRunner};

use std::{collections::HashSet, io::Cursor};

use zebra_test::prelude::*;

use hex::{FromHex, ToHex};

use super::super::*;

use crate::{
    block::Block,
    parameters::NetworkUpgrade,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::arbitrary::MAX_ARBITRARY_ITEMS,
    LedgerState,
};

/// Assert that `tx` round-trips through the wire format, or is rejected by the parser if it
/// is a coinbase transaction with Sapling spends.
fn assert_transaction_roundtrip(tx: Transaction) -> Result<(), TestCaseError> {
    let has_coinbase_sapling_spends =
        tx.is_coinbase() && tx.sapling_spends_per_anchor().count() > 0;

    let data = tx.zcash_serialize_to_vec().expect("tx should serialize");

    if has_coinbase_sapling_spends {
        // GHSA-rgwx-8r98-p34c fix: the parser now rejects coinbase
        // transactions with Sapling spends before allocating.
        data.zcash_deserialize_into::<Transaction>()
            .expect_err("coinbase with Sapling spends must be rejected");
    } else {
        let tx2: Transaction = data
            .zcash_deserialize_into()
            .expect("randomized tx should deserialize");

        prop_assert_eq![&tx, &tx2];

        let data2 = tx2
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    Ok(())
}

proptest! {
    #[test]
    fn transaction_roundtrip(tx in any::<Transaction>()) {
        let _init_guard = zebra_test::init();

        assert_transaction_roundtrip(tx)?;
    }

    /// Round-trip v6-era transactions specifically: `any::<Transaction>()` uses a mainnet-tip
    /// ledger state, and NU6.3 is not activated on Mainnet yet, so `transaction_roundtrip`
    /// does not exercise v6 transactions (including the fail-closed librustzcash round-trip
    /// in the v6 deserializer).
    #[test]
    fn transaction_roundtrip_nu6_3(
        tx in LedgerState::network_upgrade_strategy(NetworkUpgrade::Nu6_3, None, true)
            .prop_flat_map(Transaction::arbitrary_with)
    ) {
        let _init_guard = zebra_test::init();

        assert_transaction_roundtrip(tx)?;
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
    let strategy = (1..=6u32)
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

/// Make sure the arbitrary transaction strategy generates every transaction version that is
/// valid in each network-upgrade era — and no others.
///
/// This is a non-vacuity guard: when a new network upgrade is appended to the strategy dispatch
/// without adding its new transaction format, every downstream property test of that format
/// silently passes without ever seeing it. (The v4/v5-only arm did exactly that for v6/Ironwood
/// when NU6.3 support was added.)
#[test]
fn arbitrary_transaction_versions_cover_each_era() -> Result<()> {
    let _init_guard = zebra_test::init();

    const SAMPLES_PER_ERA: usize = 64;

    // Update with new network upgrades and transaction versions as needed
    let eras: &[(NetworkUpgrade, &[u32])] = &[
        (NetworkUpgrade::Genesis, &[1]),
        (NetworkUpgrade::BeforeOverwinter, &[1]),
        (NetworkUpgrade::Overwinter, &[2]),
        (NetworkUpgrade::Sapling, &[3]),
        (NetworkUpgrade::Blossom, &[4]),
        (NetworkUpgrade::Heartwood, &[4]),
        (NetworkUpgrade::Canopy, &[4]),
        (NetworkUpgrade::Nu5, &[4, 5]),
        (NetworkUpgrade::Nu6, &[4, 5]),
        (NetworkUpgrade::Nu6_1, &[4, 5]),
        (NetworkUpgrade::Nu6_2, &[4, 5]),
        (NetworkUpgrade::Nu6_3, &[4, 5, 6]),
        (NetworkUpgrade::Nu7, &[4, 5, 6]),
    ];

    // A deterministic RNG makes the "every version appears" assertions reliable: with 64
    // samples over at most 3 equally-likely versions, coverage would also hold with
    // overwhelming probability for a random seed, but there is no reason to accept flakes.
    let mut runner = TestRunner::deterministic();

    for (network_upgrade, expected_versions) in eras {
        let strategy = LedgerState::network_upgrade_strategy(*network_upgrade, None, true)
            .prop_flat_map(Transaction::arbitrary_with);

        let mut seen_versions = HashSet::new();
        let mut seen_v6_orchard_bundle = false;
        let mut seen_ironwood_bundle = false;

        for _ in 0..SAMPLES_PER_ERA {
            let transaction = strategy
                .new_tree(&mut runner)
                .expect("strategy generates values")
                .current();

            seen_versions.insert(transaction.version());

            if let Transaction::V6 {
                orchard_shielded_data,
                ironwood_shielded_data,
                ..
            } = &transaction
            {
                seen_v6_orchard_bundle |= orchard_shielded_data.is_some();
                seen_ironwood_bundle |= ironwood_shielded_data.is_some();
            }
        }

        let expected_versions: HashSet<u32> = expected_versions.iter().copied().collect();
        assert_eq!(
            seen_versions, expected_versions,
            "generated transaction versions must match the valid versions for {network_upgrade:?}",
        );

        // Non-vacuity: the generator must be able to produce the NU6.3 bundle types, otherwise
        // property tests of v6 Orchard and Ironwood behaviour pass without testing anything.
        if expected_versions.contains(&6) {
            assert!(
                seen_v6_orchard_bundle,
                "generated v6 transactions must include some v6 Orchard bundles for {network_upgrade:?}",
            );
            assert!(
                seen_ironwood_bundle,
                "generated v6 transactions must include some Ironwood bundles for {network_upgrade:?}",
            );
        }
    }

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
            Just(ledger_state.network.clone()),
            Block::arbitrary_with(ledger_state),
        )
    });

    proptest!(|((network, block) in strategy)| {
        block.check_transaction_network_upgrade_consistency(&network)?;
    });

    Ok(())
}
