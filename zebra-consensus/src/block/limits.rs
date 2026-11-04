//! Per-block limits on transparent signature operations and shielded actions.

/// The maximum number of transparent signature operations allowed in a block.
///
/// # Consensus
///
/// For every block, the sum of legacy and P2SH transparent signature operations across all
/// transactions must not exceed [20_000].
///
/// ## Notes
///
/// This rule is inherited from pre-SegWit Bitcoin, and is not explicitly stated in the Zcash
/// protocol spec. It is covered implicitly in [§7.6], which closes with "Other rules inherited from
/// Bitcoin". The inclusion of this rule is tracked in [`zcash/zips#568`].
///
/// Zebra mirrors `zcashd`'s `ConnectBlock`, which sums `GetLegacySigOpCount()` and
/// `GetP2SHSigOpCount()` per transaction before comparing against this constant.
///
/// [20_000]: <https://github.com/zcash/zcash/blob/bad7f7eadbbb3466bebe3354266c7f69f607fcfd/src/consensus/consensus.h#L30>
/// [`zcash/zips#568`]: <https://github.com/zcash/zips/issues/568>
/// [§7.6]: <https://zips.z.cash/protocol/protocol.pdf#blockheader>
pub const MAX_BLOCK_SIGOPS: u32 = 20_000;

/// The [ZIP 218] per-pool and global shielded action limits, which apply from NU7 activation.
///
/// `GlobalShieldedBudget`, `OrchardProtocolBlockActionLimit`, `SaplingBlockIOLimit` and
/// `SproutBlockJoinSplitLimit` in the ZIP. Orchard and Ironwood share the global budget and
/// have the same per-pool action limit. Sprout JoinSplits are prohibited.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
pub const GLOBAL_SHIELDED_BUDGET: usize = 330;
/// The per-pool action limit for both Orchard and Ironwood. See [`GLOBAL_SHIELDED_BUDGET`].
pub const ORCHARD_BLOCK_ACTION_LIMIT: usize = 330;
/// See [`GLOBAL_SHIELDED_BUDGET`].
pub const SAPLING_BLOCK_IO_LIMIT: usize = 300;
/// See [`GLOBAL_SHIELDED_BUDGET`].
pub const SPROUT_BLOCK_JOIN_SPLIT_LIMIT: usize = 0;

/// The shielded components that the [ZIP 218] action limits count.
///
/// Shared by the block verifier's limit check and by `getblocktemplate` transaction selection, so
/// that a template can not be built from transactions the verifier would then reject.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShieldedActionCounts {
    /// The number of Orchard actions.
    pub orchard_actions: usize,
    /// The number of Ironwood actions.
    pub ironwood_actions: usize,
    /// The number of Sapling inputs plus outputs.
    pub sapling_io: usize,
    /// The number of Sprout JoinSplits.
    pub joinsplits: usize,
}

impl ShieldedActionCounts {
    /// Returns the counts for `transaction`.
    pub fn from_transaction(transaction: &zebra_chain::transaction::Transaction) -> Self {
        Self {
            orchard_actions: transaction.orchard_actions().count(),
            ironwood_actions: transaction.ironwood_actions().count(),
            sapling_io: transaction
                .sapling_spends_count()
                .saturating_add(transaction.sapling_outputs().count()),
            joinsplits: transaction.joinsplit_count(),
        }
    }

    /// Returns these counts plus `other`.
    ///
    /// Saturating, so that a malformed count can not wrap back below a limit.
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            orchard_actions: self.orchard_actions.saturating_add(other.orchard_actions),
            ironwood_actions: self.ironwood_actions.saturating_add(other.ironwood_actions),
            sapling_io: self.sapling_io.saturating_add(other.sapling_io),
            joinsplits: self.joinsplits.saturating_add(other.joinsplits),
        }
    }

    /// Returns the total shielded cost, which each JoinSplit contributes twice to, because it
    /// produces two shielded outputs.
    pub fn shielded_cost(&self) -> usize {
        self.orchard_actions
            .saturating_add(self.ironwood_actions)
            .saturating_add(self.sapling_io)
            .saturating_add(self.joinsplits.saturating_mul(2))
    }

    /// Returns the first limit these counts exceed, as `(pool, count, limit)`.
    pub fn exceeded_limit(&self) -> Option<(&'static str, usize, usize)> {
        [
            (
                "Orchard actions",
                self.orchard_actions,
                ORCHARD_BLOCK_ACTION_LIMIT,
            ),
            (
                "Ironwood actions",
                self.ironwood_actions,
                ORCHARD_BLOCK_ACTION_LIMIT,
            ),
            (
                "Sapling inputs and outputs",
                self.sapling_io,
                SAPLING_BLOCK_IO_LIMIT,
            ),
            (
                "Sprout JoinSplits",
                self.joinsplits,
                SPROUT_BLOCK_JOIN_SPLIT_LIMIT,
            ),
            (
                "shielded actions across all pools",
                self.shielded_cost(),
                GLOBAL_SHIELDED_BUDGET,
            ),
        ]
        .into_iter()
        .find(|&(_, count, limit)| count > limit)
    }
}
