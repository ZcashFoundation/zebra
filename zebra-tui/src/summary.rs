//! Per-block statistics derived from a `getblock` response.

use crate::rpc::RpcBlock;

/// Counts of notes or coins that a block's transactions consume and create in one pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputOutputCounts {
    pub inputs: usize,
    pub outputs: usize,
}

/// A value pool's balance after the block, and the block's net change to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolBalance {
    pub id: String,
    pub balance_zat: i64,
    pub delta_zat: Option<i64>,
}

/// Statistics for one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSummary {
    pub height: u32,
    pub hash: String,
    pub time: i64,
    pub size_bytes: u64,
    pub transaction_count: usize,
    pub coinbase_count: usize,
    /// Transparent inputs exclude coinbase inputs, which spend no coin.
    pub transparent: InputOutputCounts,
    pub sprout_joinsplits: usize,
    /// Each JoinSplit spends two notes and creates two notes.
    pub sprout: InputOutputCounts,
    pub sapling: InputOutputCounts,
    pub orchard_actions: usize,
    pub ironwood_actions: usize,
    pub pools: Vec<PoolBalance>,
}

impl BlockSummary {
    /// Returns the sum of all pool changes, which is the value the block adds to the chain supply.
    pub fn net_issuance_zat(&self) -> Option<i64> {
        self.pools
            .iter()
            .map(|pool| pool.delta_zat)
            .try_fold(0i64, |total, delta| total.checked_add(delta?))
    }
}

/// Computes the statistics for `block`.
pub fn summarize(block: &RpcBlock) -> BlockSummary {
    let mut summary = BlockSummary {
        height: block.height,
        hash: block.hash.clone(),
        time: block.time,
        size_bytes: block.size,
        transaction_count: block.tx.len(),
        coinbase_count: 0,
        transparent: InputOutputCounts::default(),
        sprout_joinsplits: 0,
        sprout: InputOutputCounts::default(),
        sapling: InputOutputCounts::default(),
        orchard_actions: 0,
        ironwood_actions: 0,
        pools: block
            .value_pools
            .iter()
            .map(|pool| PoolBalance {
                id: pool.id.clone(),
                balance_zat: pool.chain_value_zat,
                delta_zat: pool.value_delta_zat,
            })
            .collect(),
    };

    for tx in &block.tx {
        let coinbase_inputs = tx.vin.iter().filter(|i| i.coinbase.is_some()).count();
        if coinbase_inputs > 0 {
            summary.coinbase_count += 1;
        }
        summary.transparent.inputs += tx.vin.len() - coinbase_inputs;
        summary.transparent.outputs += tx.vout.len();
        summary.sprout_joinsplits += tx.vjoinsplit.len();
        summary.sapling.inputs += tx.sapling_spends.len();
        summary.sapling.outputs += tx.sapling_outputs.len();
        summary.orchard_actions += tx.orchard.as_ref().map_or(0, |b| b.actions.len());
        summary.ironwood_actions += tx.ironwood.as_ref().map_or(0, |b| b.actions.len());
    }
    summary.sprout = InputOutputCounts {
        inputs: 2 * summary.sprout_joinsplits,
        outputs: 2 * summary.sprout_joinsplits,
    };

    summary
}

/// Formats a zatoshi amount as ZEC with eight decimal places.
///
/// With `signed`, non-negative amounts carry a leading `+`.
pub fn format_zec(zat: i64, signed: bool) -> String {
    let sign = match (zat < 0, signed) {
        (true, _) => "-",
        (false, true) => "+",
        (false, false) => "",
    };
    let magnitude = zat.unsigned_abs();
    format!(
        "{sign}{}.{:08}",
        group_thousands(magnitude / 100_000_000),
        magnitude % 100_000_000
    )
}

/// Formats an integer with `,` between groups of three digits.
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK_JSON: &str = r#"{
        "hash": "00ab", "height": 10, "size": 2048, "time": 1700000000,
        "tx": [
            { "vin": [{"coinbase": "03aa", "sequence": 0}],
              "vout": [{}, {}], "vjoinsplit": [], "vShieldedSpend": [], "vShieldedOutput": [],
              "orchard": {"actions": [], "valueBalanceZat": 0} },
            { "vin": [{"txid": "ff", "vout": 0}, {"txid": "ee", "vout": 1}],
              "vout": [{}], "vjoinsplit": [{}], "vShieldedSpend": [{}, {}, {}],
              "vShieldedOutput": [{}, {}],
              "orchard": {"actions": [{}, {}]},
              "ironwood": {"actions": [{}, {}, {}, {}]} },
            { }
        ],
        "valuePools": [
            {"id": "transparent", "chainValueZat": 500, "valueDeltaZat": 300},
            {"id": "sapling", "chainValueZat": 900, "valueDeltaZat": -50}
        ]
    }"#;

    fn block() -> RpcBlock {
        serde_json::from_str(BLOCK_JSON).expect("test vector is valid getblock JSON")
    }

    #[test]
    fn counts_inputs_and_outputs_per_pool() {
        let summary = summarize(&block());
        assert_eq!(summary.transaction_count, 3);
        assert_eq!(summary.coinbase_count, 1);
        assert_eq!(
            summary.transparent,
            InputOutputCounts {
                inputs: 2,
                outputs: 3
            }
        );
        assert_eq!(summary.sprout_joinsplits, 1);
        assert_eq!(
            summary.sprout,
            InputOutputCounts {
                inputs: 2,
                outputs: 2
            }
        );
        assert_eq!(
            summary.sapling,
            InputOutputCounts {
                inputs: 3,
                outputs: 2
            }
        );
        assert_eq!(summary.orchard_actions, 2);
        assert_eq!(summary.ironwood_actions, 4);
    }

    #[test]
    fn net_issuance_sums_pool_deltas() {
        assert_eq!(summarize(&block()).net_issuance_zat(), Some(250));
    }

    #[test]
    fn net_issuance_is_unknown_when_any_delta_is_missing() {
        let mut summary = summarize(&block());
        summary.pools[0].delta_zat = None;
        assert_eq!(summary.net_issuance_zat(), None);
    }

    #[test]
    fn formats_zec_amounts() {
        assert_eq!(format_zec(0, false), "0.00000000");
        assert_eq!(format_zec(0, true), "+0.00000000");
        assert_eq!(format_zec(-160_000, true), "-0.00160000");
        assert_eq!(
            format_zec(1_195_152_729_249_023, false),
            "11,951,527.29249023"
        );
        assert_eq!(format_zec(i64::MIN, false), "-92,233,720,368.54775808");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
