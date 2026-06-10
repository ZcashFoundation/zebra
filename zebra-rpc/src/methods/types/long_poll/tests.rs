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
