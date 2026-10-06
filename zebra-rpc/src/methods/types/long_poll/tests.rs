//! Tests for the long-poll ID type used by the `getblocktemplate` RPC.

use std::str::FromStr;

use zebra_chain::{block::Height, transaction};

use super::{LongPollId, LongPollInput, LONG_POLL_ID_LENGTH};

/// Check that [`LongPollInput::new`] will sort mempool transaction ids.
///
/// The mempool does not currently guarantee the order in which it will return transactions and
/// may return the same items in a different order, while the long poll id should be the same if
/// its other components are equal and no transactions have been added or removed in the mempool.
#[test]
fn long_poll_input_mempool_tx_ids_are_sorted() {
    use zebra_chain::transaction::UnminedTxId;

    let mempool_tx_ids = || {
        (0..10)
            .map(|i| transaction::Hash::from([i; 32]))
            .map(UnminedTxId::Legacy)
    };

    assert_eq!(
        LongPollInput::new(Height::MIN, Default::default(), 0.into(), mempool_tx_ids()),
        LongPollInput::new(
            Height::MIN,
            Default::default(),
            0.into(),
            mempool_tx_ids().rev()
        ),
        "long poll input should sort mempool tx ids"
    );
}

/// Check that `LongPollId::from_str` rejects strings with the correct byte length but
/// non-ASCII content, instead of panicking when its fixed byte-offset slices cross a UTF-8
/// char boundary.
///
/// Regression test for
/// [GHSA-qv2r-v3mx-f4pf](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-qv2r-v3mx-f4pf).
#[test]
fn long_poll_id_rejects_non_ascii_at_each_field_boundary() {
    // `é` is two UTF-8 bytes, so each of these strings is exactly `LONG_POLL_ID_LENGTH`
    // bytes long with the `é` straddling one of the parser's fixed byte offsets:
    // 10, 18, 28, or 38.
    let boundary_inputs = [
        // boundary 10: between tip_height and tip_hash_checksum
        format!("{}é{}", "0".repeat(9), "0".repeat(35)),
        // boundary 18: between tip_hash_checksum and max_timestamp
        format!("{}é{}", "0".repeat(17), "0".repeat(27)),
        // boundary 28: between max_timestamp and mempool_transaction_count
        format!("{}é{}", "0".repeat(27), "0".repeat(17)),
        // boundary 38: between mempool_transaction_count and
        // mempool_transaction_content_checksum
        format!("{}é{}", "0".repeat(37), "0".repeat(7)),
    ];

    for input in boundary_inputs {
        assert_eq!(
            input.len(),
            LONG_POLL_ID_LENGTH,
            "test input must be exactly LONG_POLL_ID_LENGTH bytes",
        );
        let result = LongPollId::from_str(&input);
        assert!(
            result.is_err(),
            "non-ASCII long poll id must return an error, got {result:?} for {input:?}",
        );
    }
}

/// Check that `LongPollId::from_str` round-trips a well-formed ASCII id.
#[test]
fn long_poll_id_round_trip_ascii() {
    let id = LongPollId {
        tip_height: 1234567890,
        tip_hash_checksum: 0xdeadbeef,
        max_timestamp: 4000000000,
        mempool_transaction_count: 42,
        mempool_transaction_content_checksum: 0x0badf00d,
    };
    let s = id.to_string();
    assert_eq!(s.len(), LONG_POLL_ID_LENGTH);
    assert_eq!(LongPollId::from_str(&s).unwrap(), id);
}

/// Check that `LongPollId::from_str` rejects inputs whose byte length does not match
/// `LONG_POLL_ID_LENGTH`.
#[test]
fn long_poll_id_rejects_wrong_length() {
    assert!(LongPollId::from_str("").is_err());
    assert!(LongPollId::from_str(&"0".repeat(LONG_POLL_ID_LENGTH - 1)).is_err());
    assert!(LongPollId::from_str(&"0".repeat(LONG_POLL_ID_LENGTH + 1)).is_err());
}

/// A long poll id with arbitrary but fixed field values, used as the `old` id that a miner
/// already holds work for.
fn old_long_poll_id() -> LongPollId {
    LongPollId {
        tip_height: 1_000_000,
        tip_hash_checksum: 0xdeadbeef,
        max_timestamp: 4_000_000_000,
        mempool_transaction_count: 7,
        mempool_transaction_content_checksum: 0x0badf00d,
    }
}

/// Check that unchanged work stays submittable.
///
/// If nothing has changed, a miner's queued shares are still valid for the new template.
#[test]
fn submit_old_is_true_for_an_unchanged_id() {
    let old = old_long_poll_id();

    assert!(
        old.submit_old(&old),
        "an unchanged long poll id must keep old work submittable",
    );
}

/// Check that any change to the block header invalidates old work.
///
/// `tip_height`, `tip_hash_checksum` and `max_timestamp` all feed into the block header, so a
/// change to any of them means every queued share is mining on a header that can no longer be
/// part of the best chain.
#[test]
fn submit_old_is_false_when_a_header_field_changes() {
    let old = old_long_poll_id();

    let changed_tip_height = LongPollId {
        tip_height: old.tip_height + 1,
        ..old
    };
    assert!(
        !changed_tip_height.submit_old(&old),
        "a changed tip height must invalidate old work",
    );

    let changed_tip_hash_checksum = LongPollId {
        tip_hash_checksum: old.tip_hash_checksum + 1,
        ..old
    };
    assert!(
        !changed_tip_hash_checksum.submit_old(&old),
        "a changed tip hash checksum must invalidate old work",
    );

    let changed_max_timestamp = LongPollId {
        max_timestamp: old.max_timestamp + 1,
        ..old
    };
    assert!(
        !changed_max_timestamp.submit_old(&old),
        "a changed max timestamp must invalidate old work",
    );
}

/// Check that mempool-only changes keep old work submittable.
///
/// A miner is not obliged to include newly arrived transactions, so its queued shares are still
/// valid blocks when only the mempool has changed.
#[test]
fn submit_old_is_true_when_only_the_mempool_changes() {
    let old = old_long_poll_id();

    let changed_count = LongPollId {
        mempool_transaction_count: old.mempool_transaction_count + 1,
        ..old
    };
    assert!(
        changed_count.submit_old(&old),
        "a changed mempool transaction count must keep old work submittable",
    );

    let changed_content_checksum = LongPollId {
        mempool_transaction_content_checksum: old.mempool_transaction_content_checksum + 1,
        ..old
    };
    assert!(
        changed_content_checksum.submit_old(&old),
        "a changed mempool content checksum must keep old work submittable",
    );
}

/// Check that a header change invalidates old work even when the mempool changed as well.
///
/// The mempool fields must never mask a tip change: that would tell miners to keep submitting
/// shares built on a stale parent.
#[test]
fn submit_old_is_false_when_the_tip_and_the_mempool_both_change() {
    let old = old_long_poll_id();

    let changed_both = LongPollId {
        tip_height: old.tip_height + 1,
        tip_hash_checksum: old.tip_hash_checksum + 1,
        mempool_transaction_count: old.mempool_transaction_count + 1,
        mempool_transaction_content_checksum: old.mempool_transaction_content_checksum + 1,
        ..old
    };

    assert!(
        !changed_both.submit_old(&old),
        "a mempool change must not mask a tip change",
    );
}
