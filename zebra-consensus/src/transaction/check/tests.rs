//! Spend-conflict parity across stack and hash-table duplicate checks.

use std::{borrow::Cow, collections::HashSet};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
    transparent,
};

use super::{check_for_duplicates, spend_conflicts};
use crate::error::TransactionError;

fn legacy_spend_conflicts(tx: &Transaction) -> Result<(), TransactionError> {
    macro_rules! check {
        ($items:expr, $variant:path) => {{
            let mut seen = HashSet::new();
            for item in $items {
                if let Some(duplicate) = seen.replace(item) {
                    return Err($variant(duplicate));
                }
            }
        }};
    }
    check!(
        tx.spent_outpoints(),
        TransactionError::DuplicateTransparentSpend
    );
    check!(
        tx.sprout_nullifiers(),
        TransactionError::DuplicateSproutNullifier
    );
    check!(
        tx.sapling_nullifiers(),
        TransactionError::DuplicateSaplingNullifier
    );
    check!(
        tx.orchard_nullifiers(),
        TransactionError::DuplicateOrchardNullifier
    );
    check!(
        tx.ironwood_nullifiers(),
        TransactionError::DuplicateIronwoodNullifier
    );
    Ok(())
}

#[test]
fn borrowed_small_and_large_groups_report_the_first_duplicate() {
    for len in 0u8..12 {
        let unique: Vec<_> = (0..len).map(|n| [n; 32]).collect();
        assert!(
            check_for_duplicates(unique.iter().map(Cow::Borrowed), |nf| {
                TransactionError::DuplicateSaplingNullifier(nf.into())
            })
            .is_ok()
        );
        for position in 0..unique.len() {
            let mut duplicated = unique.clone();
            duplicated.push(unique[position]);
            // A later duplicate must not displace the first conflict's error value.
            duplicated.push([255; 32]);
            duplicated.push([255; 32]);
            let expected = TransactionError::DuplicateSaplingNullifier(unique[position].into());
            assert_eq!(
                check_for_duplicates(duplicated.iter().map(Cow::Borrowed), |nf| {
                    TransactionError::DuplicateSaplingNullifier(nf.into())
                }),
                Err(expected.clone()),
            );
            assert_eq!(
                check_for_duplicates(duplicated.into_iter().map(Cow::<[u8; 32]>::Owned), |nf| {
                    TransactionError::DuplicateSaplingNullifier(nf.into())
                }),
                Err(expected),
            );
        }
    }
}

#[test]
fn stack_and_hash_groups_stop_at_the_first_conflict() {
    for len in [2u8, 7] {
        let values = (0..len).map(|n| [n; 32]);
        let duplicate = [len - 1; 32];
        let items = values
            .chain(std::iter::once(duplicate))
            .chain(std::iter::once_with(|| {
                panic!("must stop after a duplicate")
            }))
            .map(Cow::<[u8; 32]>::Owned);
        assert_eq!(
            check_for_duplicates(items, |nf| TransactionError::DuplicateSproutNullifier(
                nf.into()
            )),
            Err(TransactionError::DuplicateSproutNullifier(duplicate.into())),
        );
    }
}

#[test]
fn transparent_spend_errors_match_before_and_after_hash_table_cutover() {
    for len in [1u8, 4, 5, 8] {
        let unique: Vec<_> = (0..len)
            .map(|index| transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: zebra_chain::transaction::Hash([index + 1; 32]),
                    index: u32::from(index),
                },
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            })
            .collect();
        for position in 0..unique.len() {
            let mut inputs = unique.clone();
            inputs.push(unique[position].clone());
            let tx =
                Transaction::test_v4(inputs, Vec::new(), LockTime::Height(Height(0)), Height(1));
            let expected =
                TransactionError::DuplicateTransparentSpend(unique[position].outpoint().unwrap());
            assert_eq!(legacy_spend_conflicts(&tx), Err(expected.clone()));
            assert_eq!(spend_conflicts(&tx), Err(expected));
        }
    }
}

#[test]
fn spend_results_match_legacy_checks_for_every_network_vector() {
    for network in Network::iter() {
        for (_, bytes) in network.block_iter() {
            let block: Block = bytes.zcash_deserialize_into().unwrap();
            for tx in block.transactions {
                assert_eq!(spend_conflicts(&tx), legacy_spend_conflicts(&tx));
            }
        }
    }
}
