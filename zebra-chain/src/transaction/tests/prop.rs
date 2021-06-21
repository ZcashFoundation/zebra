//! Randomised property tests for transactions.

use proptest::prelude::*;

use std::io::Cursor;

use zebra_test::prelude::*;

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
        zebra_test::init();

        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2 = data.zcash_deserialize_into().expect("randomized tx should deserialize");

        prop_assert_eq![&tx, &tx2];

        let data2 = tx2
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        prop_assert_eq![data, data2, "data must be equal if structs are equal"];
    }

    #[test]
    fn transaction_hash_display_fromstr_roundtrip(hash in any::<Hash>()) {
        zebra_test::init();

        let display = format!("{}", hash);
        let parsed = display.parse::<Hash>().expect("hash should parse");
        prop_assert_eq!(hash, parsed);
    }

    #[test]
    fn locktime_roundtrip(locktime in any::<LockTime>()) {
        zebra_test::init();

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
    zebra_test::init();

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
    zebra_test::init();

    // Update with new transaction versions as needed
    let strategy = LedgerState::coinbase_strategy(None, 5, true).prop_flat_map(|ledger_state| {
        (
            Just(ledger_state.network),
            Block::arbitrary_with(ledger_state),
        )
    });

    proptest!(|((network, block) in strategy)| {
        // TODO: replace with check_transaction_network_upgrade from #2343
        let block_network_upgrade = NetworkUpgrade::current(network, block.coinbase_height().unwrap());
        for transaction in block.transactions {
            if let Transaction::V5 { network_upgrade, .. } = transaction.as_ref() {
                prop_assert_eq!(network_upgrade, &block_network_upgrade);
            }
        }
    });

    Ok(())
}
