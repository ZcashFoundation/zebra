//! Duplicate-spend regression cases.

use zebra_chain::{transaction, transparent::OutPoint};

use super::{check_for_duplicates, TransactionError};

fn outpoint(index: u32) -> OutPoint {
    OutPoint {
        hash: transaction::Hash([0; 32]),
        index,
    }
}

#[test]
fn duplicate_spend_iterator_preserves_the_first_conflict() {
    for (indices, duplicate) in [
        (vec![], None),
        (vec![0], None),
        (vec![0, 1], None),
        (vec![0, 0], Some(0)),
        (vec![0, 1, 2, 1, 0], Some(1)),
        (vec![0, 1, 0], Some(0)),
    ] {
        let result = check_for_duplicates(
            indices.into_iter().map(outpoint),
            TransactionError::DuplicateTransparentSpend,
        );
        assert_eq!(
            result,
            duplicate.map_or(Ok(()), |index| {
                Err(TransactionError::DuplicateTransparentSpend(outpoint(index)))
            }),
        );
    }
}
