//! Mempool statistics derived from `getmempoolinfo` and `getrawmempool` responses.

use crate::rpc::{RpcMempoolEntry, RpcMempoolInfo};

/// The largest block that consensus allows, in bytes.
pub const MAX_BLOCK_BYTES: u64 = 2_000_000;

/// Statistics for the node's mempool.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MempoolSummary {
    pub transaction_count: usize,
    pub bytes: u64,
    pub memory_usage_bytes: u64,
    pub total_fee_zat: i64,
    /// Fee rates in zatoshis per 1000 bytes. `None` when the mempool is empty.
    pub fee_rate: Option<FeeRateRange>,
    /// The entry time of the transaction that has waited longest, in Unix seconds.
    pub oldest_entry_time: Option<i64>,
    /// Transactions that spend outputs of other mempool transactions.
    pub dependent_count: usize,
}

/// The lowest, median, and highest fee rate, in zatoshis per 1000 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeRateRange {
    pub min: u64,
    pub median: u64,
    pub max: u64,
}

impl MempoolSummary {
    /// Returns the number of full blocks that the mempool's transactions would fill, rounded up.
    pub fn blocks_to_clear(&self) -> u64 {
        self.bytes.div_ceil(MAX_BLOCK_BYTES)
    }
}

/// Converts a ZEC amount from a JSON number to zatoshis, rounding to the nearest zatoshi.
///
/// The conversion is exact for every amount up to the 21 million ZEC supply,
/// because such amounts in zatoshis are below 2^53.
pub fn zec_to_zat(zec: f64) -> i64 {
    // The rounded value is an integer, and `as` saturates at the i64 bounds.
    (zec * 100_000_000.0).round() as i64
}

/// Computes the mempool statistics from the node's `info` and its verbose transaction `entries`.
pub fn summarize_mempool<'a>(
    info: &RpcMempoolInfo,
    entries: impl IntoIterator<Item = &'a RpcMempoolEntry>,
) -> MempoolSummary {
    let mut total_fee_zat: i64 = 0;
    let mut fee_rates = Vec::new();
    let mut oldest_entry_time: Option<i64> = None;
    let mut dependent_count = 0;

    for entry in entries {
        let fee_zat = zec_to_zat(entry.fee);
        total_fee_zat = total_fee_zat.saturating_add(fee_zat);
        let fee_rate = u64::try_from(fee_zat.max(0))
            .unwrap_or(0)
            .saturating_mul(1000)
            .checked_div(entry.size);
        if let Some(fee_rate) = fee_rate {
            fee_rates.push(fee_rate);
        }
        oldest_entry_time = Some(oldest_entry_time.map_or(entry.time, |t| t.min(entry.time)));
        if !entry.depends.is_empty() {
            dependent_count += 1;
        }
    }

    fee_rates.sort_unstable();
    let fee_rate = match (fee_rates.first(), fee_rates.last()) {
        (Some(&min), Some(&max)) => Some(FeeRateRange {
            min,
            median: fee_rates[fee_rates.len() / 2],
            max,
        }),
        _ => None,
    };

    MempoolSummary {
        transaction_count: info.size,
        bytes: info.bytes,
        memory_usage_bytes: info.usage,
        total_fee_zat,
        fee_rate,
        oldest_entry_time,
        dependent_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(size: u64, fee: f64, time: i64, depends: usize) -> RpcMempoolEntry {
        RpcMempoolEntry {
            size,
            fee,
            time,
            depends: vec![serde::de::IgnoredAny; depends],
        }
    }

    #[test]
    fn summarizes_fees_ages_and_dependencies() {
        let info = RpcMempoolInfo {
            size: 3,
            bytes: 4_500_000,
            usage: 9_000_000,
        };
        let entries = [
            entry(1000, 0.0001, 300, 0),
            entry(2000, 0.0003, 100, 1),
            entry(500, 0.00002, 200, 0),
        ];
        let summary = summarize_mempool(&info, &entries);
        assert_eq!(summary.transaction_count, 3);
        assert_eq!(summary.total_fee_zat, 42_000);
        assert_eq!(
            summary.fee_rate,
            Some(FeeRateRange {
                min: 4_000,
                median: 10_000,
                max: 15_000
            })
        );
        assert_eq!(summary.oldest_entry_time, Some(100));
        assert_eq!(summary.dependent_count, 1);
        assert_eq!(summary.blocks_to_clear(), 3);
    }

    #[test]
    fn empty_mempool_has_no_fee_rates_or_age() {
        let info = RpcMempoolInfo {
            size: 0,
            bytes: 0,
            usage: 0,
        };
        let summary = summarize_mempool(&info, &[]);
        assert_eq!(summary.fee_rate, None);
        assert_eq!(summary.oldest_entry_time, None);
        assert_eq!(summary.blocks_to_clear(), 0);
    }

    #[test]
    fn converts_json_zec_to_exact_zatoshis() {
        assert_eq!(zec_to_zat(0.0001), 10_000);
        assert_eq!(zec_to_zat(0.00000001), 1);
        assert_eq!(zec_to_zat(21_000_000.0), 2_100_000_000_000_000);
        assert_eq!(zec_to_zat(0.1 + 0.2), 30_000_000);
    }
}
