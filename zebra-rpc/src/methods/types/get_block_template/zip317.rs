//! Mempool transaction selection for block templates.
//!
//! [`select_mempool_transactions()`] reserves the space that the block header, the transaction
//! count and the coinbase transaction need, and [`select_by_fee_rate()`] fills the rest of the
//! block with mempool transactions.
//!
//! A mempool transaction can spend outputs of other mempool transactions, which then have to go in
//! the block before it. So each transaction is rated together with the mempool transactions it
//! depends on, directly or indirectly, that are not in the template yet: by their total miner fee
//! per unit of their total [ZIP 401] cost. The best-rated transaction goes in along with those
//! dependencies, which changes the rates of the transactions that depend on them. That way a
//! transaction that pays a high fee can bring in dependencies that pay low fees.
//!
//! Rates are compared exactly. Equal rates go in the order the transactions entered the mempool,
//! then in transaction ID order, so the same mempool always gives the same template.
//!
//! These rules are not consensus-critical, so other node implementations can fill templates
//! differently.
//!
//! [ZIP 401]: https://zips.z.cash/zip-0401

use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeSet, HashMap, HashSet},
};

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Header, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    serialization::{CompactSizeMessage, ZcashDeserializeInto, ZcashSerialize},
    transaction::{
        self, zip317::BLOCK_UNPAID_ACTION_LIMIT, Transaction, VerifiedUnminedTx,
        MIN_TRANSPARENT_TX_SIZE,
    },
};
use zebra_consensus::{ShieldedActionCounts, MAX_BLOCK_SIGOPS};
use zebra_node_services::mempool::TransactionDependencies;

use super::CoinbaseCache;
use crate::methods::types::transaction::TransactionTemplate;

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::methods::types::get_block_template::InBlockTxDependenciesDepth;

use super::MinerParams;

/// Used in the return type of [`select_mempool_transactions()`] for test compilations.
#[cfg(test)]
type SelectedMempoolTx = (InBlockTxDependenciesDepth, VerifiedUnminedTx);

/// Used in the return type of [`select_mempool_transactions()`] for non-test compilations.
#[cfg(not(test))]
type SelectedMempoolTx = VerifiedUnminedTx;

/// Selects mempool transactions for block production, using a fake coinbase transaction to
/// reserve the coinbase's share of the block limits, and [`select_by_fee_rate()`] to fill the rest
/// of the block.
///
/// The sizing coinbase must have at least the bytes, sigops, and shielded components of the
/// fee-bearing coinbase. Both use the same subsidy and miner parameters; changing only the
/// reward amount does not change these costs.
///
/// Returns selected transactions from `mempool_txs`, in the order they were selected.
#[allow(clippy::too_many_arguments)]
pub fn select_mempool_transactions(
    net: &Network,
    height: Height,
    miner_params: &MinerParams,
    mempool_txs: Vec<VerifiedUnminedTx>,
    mempool_tx_deps: TransactionDependencies,
    coinbase_cache: Option<&CoinbaseCache>,
    parent_nsm_value_balance: Option<Amount<NonNegative>>,
) -> Vec<SelectedMempoolTx> {
    if let Some(cache) = coinbase_cache {
        cache.select(height, parent_nsm_value_balance);
    }

    // Use a fake coinbase transaction to break the dependency between transaction
    // selection, the miner fee, and the fee payment in the coinbase transaction.
    //
    // The fake coinbase only depends on the height, miner parameters, and parent NSM value
    // balance (its fee is always zero), so it's constant per block. Reuse the same per-block cache
    // as the real coinbase to avoid re-proving a shielded coinbase on every `getblocktemplate`
    // call just to read its size.
    let fake_coinbase_tx = coinbase_cache
        .and_then(|cache| cache.get(height, Amount::zero(), parent_nsm_value_balance))
        .unwrap_or_else(|| {
            let cb = TransactionTemplate::new_coinbase_with_parent_pools(
                net,
                height,
                miner_params,
                Amount::zero(),
                parent_nsm_value_balance,
            )
            .expect("valid coinbase transaction template");
            if let Some(cache) = coinbase_cache {
                cache.store(height, Amount::zero(), parent_nsm_value_balance, cb.clone());
            }
            cb
        });

    // Set up limit tracking
    let mut remaining_block_bytes: usize = MAX_BLOCK_BYTES.try_into().expect("fits in memory");
    let mut remaining_block_sigops = MAX_BLOCK_SIGOPS;
    let remaining_block_unpaid_actions: u32 = BLOCK_UNPAID_ACTION_LIMIT;
    let mut remaining_shielded = ShieldedBudget::new(net, height);

    // `MAX_BLOCK_BYTES` limits the whole serialized block, so reserve space for the block header
    // and the transaction count before budgeting transactions, or the assembled block could
    // exceed the consensus size limit (GHSA-95m2-vx53-v2jw).
    remaining_block_bytes -= Header::serialized_size(net);
    remaining_block_bytes -= max_transaction_count_size();

    // Reserve every coinbase cost before selecting candidates. Checked subtraction also handles
    // a coinbase that exhausts the byte or sigop budget without wrapping.
    let Some(block_bytes) = remaining_block_bytes.checked_sub(fake_coinbase_tx.data.as_ref().len())
    else {
        return Vec::new();
    };
    let Some(block_sigops) = remaining_block_sigops.checked_sub(fake_coinbase_tx.sigops) else {
        return Vec::new();
    };
    let coinbase: Transaction = fake_coinbase_tx
        .data
        .as_ref()
        .zcash_deserialize_into()
        .expect("locally constructed coinbase must deserialize");
    if !remaining_shielded.try_add(&coinbase) {
        return Vec::new();
    }
    remaining_block_bytes = block_bytes;
    remaining_block_sigops = block_sigops;

    select_by_fee_rate(
        mempool_txs,
        &mempool_tx_deps,
        BlockSpace {
            bytes: remaining_block_bytes,
            sigops: remaining_block_sigops,
            unpaid_actions: remaining_block_unpaid_actions,
            shielded: remaining_shielded,
        },
    )
}

/// Returns the maximum possible serialized size of a block's transaction count, in bytes.
///
/// The transaction count is a CompactSize whose width grows with the count. A serialized
/// transaction takes at least [`MIN_TRANSPARENT_TX_SIZE`] bytes, so a block can never contain
/// more than `MAX_BLOCK_BYTES / MIN_TRANSPARENT_TX_SIZE` transactions, which bounds the width.
fn max_transaction_count_size() -> usize {
    let max_transaction_count: usize = (MAX_BLOCK_BYTES / MIN_TRANSPARENT_TX_SIZE)
        .try_into()
        .expect("fits in memory");

    let max_transaction_count = CompactSizeMessage::try_from(max_transaction_count)
        .expect("the maximum transaction count is below the CompactSize message limit");

    max_transaction_count
        .zcash_serialize_to_vec()
        .expect("serialization into a vec can't fail")
        .len()
}

/// A block template with this many unused bytes or fewer is full enough: no more transactions are
/// tried, even if a small one would still fit.
const UNUSED_BYTES_WHEN_FULL: usize = 3_000;

/// A block template is also full enough after this many tries in a row fail.
///
/// Signature operations or [ZIP 218] shielded actions can run out while a block still has many
/// unused bytes. Then most of the remaining candidates fail, and this limit ends the search for the
/// few that could still fit.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
const FAILED_TRIES_BEFORE_FULL: usize = 1_000;

/// Fills `space` with transactions from `mempool_txs`, best fee rate first.
///
/// Each try takes the first candidate in [`QueueKey`] order, and adds it together with the
/// unselected mempool transactions it depends on, or none of them if they don't all fit. Either way
/// the candidate leaves the queue, and it only goes back in when one of the transactions it depends
/// on is added.
///
/// `mempool_tx_deps` lists the mempool transactions that each transaction spends outputs from. A
/// transaction that depends on one that is not in `mempool_txs` is never selected.
///
/// Rating a candidate visits every unselected transaction it depends on, and adding transactions
/// rates every unselected transaction that depends on them again. So the work grows quickly with
/// the number of mempool transactions that depend on each other, and this relies on the mempool
/// keeping that number small.
///
/// Returns the selected transactions in the order they were added, which puts each one after the
/// transactions it depends on.
fn select_by_fee_rate(
    mempool_txs: Vec<VerifiedUnminedTx>,
    mempool_tx_deps: &TransactionDependencies,
    space: BlockSpace,
) -> Vec<SelectedMempoolTx> {
    let mut selection = FeeRateSelection::new(mempool_txs, mempool_tx_deps, space);
    let mut failed_tries = 0;

    while selection.space.bytes > UNUSED_BYTES_WHEN_FULL && failed_tries < FAILED_TRIES_BEFORE_FULL
    {
        let Some(best) = selection.queue.pop_first() else {
            break;
        };

        if selection.try_add(best.id) {
            failed_tries = 0;
        } else {
            failed_tries += 1;
        }
    }

    selection.into_selected()
}

/// The space left in a block template under each block limit.
#[derive(Clone)]
struct BlockSpace {
    /// The bytes left under `MAX_BLOCK_BYTES`.
    bytes: usize,

    /// The signature operations left under `MAX_BLOCK_SIGOPS`.
    sigops: u32,

    /// The unpaid actions left under the [ZIP 317] `BLOCK_UNPAID_ACTION_LIMIT`.
    ///
    /// [ZIP 317]: https://zips.z.cash/zip-0317#block-production
    unpaid_actions: u32,

    /// The [ZIP 218] shielded actions that are already in the block template.
    ///
    /// [ZIP 218]: https://zips.z.cash/zip-0218
    shielded: ShieldedBudget,
}

impl BlockSpace {
    /// Returns the space left after adding `txs` in order, or `None` if one of them would go over a
    /// block limit that [`TryUpdateBlockLimits`] checks.
    fn after_adding<'tx>(
        &self,
        txs: impl IntoIterator<Item = &'tx VerifiedUnminedTx>,
    ) -> Option<Self> {
        let mut space = self.clone();

        for tx in txs {
            if !tx.try_update_block_template_limits(
                &mut space.bytes,
                &mut space.sigops,
                &mut space.unpaid_actions,
                &mut space.shielded,
            ) {
                return None;
            }
        }

        Some(space)
    }
}

/// A fee rate: miner fees in zatoshis per unit of [ZIP 401] cost.
///
/// Fee rates are ordered by their exact value, so equal fractions like 1/2 and 2/4 are equal.
///
/// [ZIP 401]: https://zips.z.cash/zip-0401
#[derive(Clone, Copy, Debug)]
struct FeeRate {
    /// The total miner fee, in zatoshis.
    fees: u64,

    /// The total cost, which is not zero, because every transaction costs at least 10,000.
    costs: u64,
}

impl Ord for FeeRate {
    fn cmp(&self, other: &Self) -> Ordering {
        // `a / b` and `c / d` compare like `a * d` and `c * b` when `b` and `d` are positive. A
        // product of two `u64`s always fits in a `u128`, so nothing is rounded.
        let scaled_self = u128::from(self.fees) * u128::from(other.costs);
        let scaled_other = u128::from(other.fees) * u128::from(self.costs);

        scaled_self.cmp(&scaled_other)
    }
}

impl PartialOrd for FeeRate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for FeeRate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for FeeRate {}

/// The place of a candidate in the queue, where the lowest key is tried first.
///
/// Keys are compared field by field: a higher fee rate goes first, then an earlier mempool entry
/// time, with an unknown time before any known time, then a lower mined ID, comparing its bytes in
/// internal order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct QueueKey {
    /// The fee rate of the candidate and the unselected transactions it depends on.
    rate: Reverse<FeeRate>,

    /// The time the candidate entered the mempool, if it is known.
    time: Option<DateTime<Utc>>,

    /// The mined ID of the candidate.
    id: transaction::Hash,
}

/// A candidate transaction in the queue.
struct Candidate {
    /// The candidate's place in the queue.
    key: QueueKey,

    /// The unselected transactions that the candidate depends on, and then the candidate itself,
    /// in [`Links::block_order()`].
    block_order: Vec<transaction::Hash>,
}

/// The links between mempool transactions that spend outputs of other mempool transactions, by
/// mined ID.
#[derive(Default)]
struct Links {
    /// The mempool transactions that each transaction spends outputs from, including any that are
    /// not candidates.
    dependencies: HashMap<transaction::Hash, Vec<transaction::Hash>>,

    /// The mempool transactions that spend outputs of each transaction.
    dependents: HashMap<transaction::Hash, Vec<transaction::Hash>>,
}

impl Links {
    /// Returns the links in `deps`.
    ///
    /// The dependents are worked out from the dependencies, so the two always agree, and every
    /// transaction that depends on an added one is rated again.
    fn new(deps: &TransactionDependencies) -> Self {
        let mut links = Self::default();

        for (dependent, dependencies) in deps.dependencies() {
            for dependency in dependencies {
                links
                    .dependents
                    .entry(*dependency)
                    .or_default()
                    .push(*dependent);
            }

            links
                .dependencies
                .insert(*dependent, dependencies.iter().copied().collect());
        }

        links
    }

    /// Returns the mempool transactions that `id` spends outputs from.
    fn dependencies_of(&self, id: &transaction::Hash) -> &[transaction::Hash] {
        self.dependencies
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns the mempool transactions that spend outputs of `id`.
    fn dependents_of(&self, id: &transaction::Hash) -> &[transaction::Hash] {
        self.dependents
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns `txs` in an order that a block can have them in: each one after the transactions in
    /// `txs` that it spends outputs from, and the lowest mined ID first among the ones that can go
    /// next.
    ///
    /// Returns `None` if some of `txs` spend outputs from each other in a cycle, so they have no
    /// such order.
    fn block_order(&self, txs: &HashSet<transaction::Hash>) -> Option<Vec<transaction::Hash>> {
        // For each transaction in `txs`, how many of the others it still waits for, and which of
        // the others wait for it.
        let mut waits_for: HashMap<transaction::Hash, usize> = HashMap::with_capacity(txs.len());
        let mut waited_on_by: HashMap<transaction::Hash, Vec<transaction::Hash>> = HashMap::new();

        for &tx in txs {
            let mut count = 0;

            for &dependency in self.dependencies_of(&tx) {
                if txs.contains(&dependency) {
                    count += 1;
                    waited_on_by.entry(dependency).or_default().push(tx);
                }
            }

            waits_for.insert(tx, count);
        }

        let mut ready: BTreeSet<transaction::Hash> = waits_for
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(tx, _)| *tx)
            .collect();
        let mut order = Vec::with_capacity(txs.len());

        while let Some(next) = ready.pop_first() {
            order.push(next);

            for waiting in waited_on_by
                .get(&next)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                // Every transaction in `waited_on_by` is in `txs`, so it has a count.
                if let Some(count) = waits_for.get_mut(waiting) {
                    // Each link is counted once, and `next` leaves `ready` only once, so the count
                    // can't go below zero.
                    *count -= 1;

                    if *count == 0 {
                        ready.insert(*waiting);
                    }
                }
            }
        }

        (order.len() == txs.len()).then_some(order)
    }
}

/// A block template while [`select_by_fee_rate()`] fills it.
struct FeeRateSelection {
    /// The links between the mempool transactions.
    links: Links,

    /// The mempool transactions that are not in the template yet, by mined ID.
    pending: HashMap<transaction::Hash, VerifiedUnminedTx>,

    /// The mined IDs of the transactions in the template.
    in_template: HashSet<transaction::Hash>,

    /// The transactions in the template, in the order they were added.
    added: Vec<VerifiedUnminedTx>,

    /// The space left in the template.
    space: BlockSpace,

    /// The candidates that can be tried, in the order they are tried.
    queue: BTreeSet<QueueKey>,

    /// The candidates in `queue`, by mined ID.
    candidates: HashMap<transaction::Hash, Candidate>,
}

impl FeeRateSelection {
    /// Returns an empty template with `space` left, and every transaction in `mempool_txs` that can
    /// be added to it in the queue.
    fn new(
        mempool_txs: Vec<VerifiedUnminedTx>,
        deps: &TransactionDependencies,
        space: BlockSpace,
    ) -> Self {
        let pending: HashMap<_, _> = mempool_txs
            .into_iter()
            .map(|tx| (tx.transaction.id.mined_id(), tx))
            .collect();
        let ids: Vec<_> = pending.keys().copied().collect();

        let mut selection = Self {
            links: Links::new(deps),
            pending,
            in_template: HashSet::new(),
            added: Vec::new(),
            space,
            queue: BTreeSet::new(),
            candidates: HashMap::new(),
        };

        for id in ids {
            selection.rate(id);
        }

        selection
    }

    /// Rates `id` against the transactions in the template now, and puts it in the queue in place
    /// of its old rate, if it has one.
    ///
    /// Leaves it out of the queue if it can't be added to the template, see [`Self::candidate()`].
    fn rate(&mut self, id: transaction::Hash) {
        if let Some(old) = self.candidates.remove(&id) {
            self.queue.remove(&old.key);
        }

        if let Some(candidate) = self.candidate(id) {
            self.queue.insert(candidate.key);
            self.candidates.insert(id, candidate);
        }
    }

    /// Returns `id` as a candidate, with the unselected transactions it depends on.
    ///
    /// Returns `None` if `id` is not pending, if it depends on a transaction that is neither in the
    /// template nor pending, if it depends on transactions that spend outputs from each other in a
    /// cycle, or if the fees or costs add up to more than a `u64` can hold.
    fn candidate(&self, id: transaction::Hash) -> Option<Candidate> {
        let tx = self.pending.get(&id)?;

        // `id` and the pending transactions it depends on, with their total fees and costs.
        let mut needed = HashSet::from([id]);
        let mut fees = u64::from(tx.miner_fee);
        let mut costs = tx.cost();

        let mut unexplored = vec![id];
        while let Some(next) = unexplored.pop() {
            for dependency in self.links.dependencies_of(&next) {
                // A transaction in the template has all its dependencies in the template, so
                // there is nothing more to explore there.
                if self.in_template.contains(dependency) || needed.contains(dependency) {
                    continue;
                }

                let dependency_tx = self.pending.get(dependency)?;
                fees = fees.checked_add(u64::from(dependency_tx.miner_fee))?;
                costs = costs.checked_add(dependency_tx.cost())?;

                needed.insert(*dependency);
                unexplored.push(*dependency);
            }
        }

        Some(Candidate {
            key: QueueKey {
                rate: Reverse(FeeRate { fees, costs }),
                time: tx.time,
                id,
            },
            block_order: self.links.block_order(&needed)?,
        })
    }

    /// Tries to add the candidate `id` together with the unselected transactions it depends on.
    ///
    /// Returns `false`, and leaves the template unchanged, if any of them would go over a block
    /// limit.
    fn try_add(&mut self, id: transaction::Hash) -> bool {
        let candidate = self
            .candidates
            .remove(&id)
            .expect("every key in the queue belongs to a candidate");

        let txs = candidate.block_order.iter().map(|tx_id| {
            self.pending
                .get(tx_id)
                .expect("candidates only list pending transactions")
        });
        let Some(space) = self.space.after_adding(txs) else {
            return false;
        };
        self.space = space;

        for tx_id in &candidate.block_order {
            // A dependency can be a candidate too.
            if let Some(dependency) = self.candidates.remove(tx_id) {
                self.queue.remove(&dependency.key);
            }

            let tx = self
                .pending
                .remove(tx_id)
                .expect("candidates only list pending transactions");
            self.in_template.insert(*tx_id);
            self.added.push(tx);
        }

        self.rate_dependents_again(&candidate.block_order);

        true
    }

    /// Rates again every transaction that depends on `added`, directly or indirectly, because some
    /// of the transactions it depends on are now in the template.
    fn rate_dependents_again(&mut self, added: &[transaction::Hash]) {
        let mut reached = HashSet::new();
        let mut generation = added.to_vec();

        while !generation.is_empty() {
            generation = generation
                .iter()
                .flat_map(|id| self.links.dependents_of(id))
                .filter(|dependent| reached.insert(**dependent))
                .copied()
                .collect();
        }

        for id in reached {
            self.rate(id);
        }
    }

    /// Returns the transactions in the template, in the order they were added.
    #[cfg(not(test))]
    fn into_selected(self) -> Vec<SelectedMempoolTx> {
        self.added
    }

    /// Returns the transactions in the template, in the order they were added, each with its
    /// depth: 0 if it spends no outputs of other transactions in the template, and otherwise one
    /// more than the deepest transaction in the template that it spends outputs from.
    #[cfg(test)]
    fn into_selected(self) -> Vec<SelectedMempoolTx> {
        let mut depths: HashMap<transaction::Hash, InBlockTxDependenciesDepth> = HashMap::new();

        self.added
            .into_iter()
            .map(|tx| {
                let id = tx.transaction.id.mined_id();
                let depth = self
                    .links
                    .dependencies_of(&id)
                    .iter()
                    .map(|dependency| {
                        *depths.get(dependency).expect(
                            "a transaction is added after all the transactions it depends on",
                        )
                    })
                    .max()
                    .map_or(0, |deepest| deepest + 1);

                depths.insert(id, depth);
                (depth, tx)
            })
            .collect()
    }
}

/// Tracks a block template's [ZIP 218] shielded action budget.
///
/// The limits only apply from NU7, so before then `applies` is false and every transaction fits.
///
/// Template selection has to apply the same limits as the block verifier: without this, a mempool
/// holding more than a block's worth of shielded actions would produce a template that
/// `check::shielded_action_limits_are_valid()` rejects, so a Zebra-backed miner would mine invalid
/// blocks and a `getblocktemplate` proposal round-trip would fail.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
#[derive(Clone)]
struct ShieldedBudget {
    /// Whether the ZIP 218 limits apply at this block's height.
    applies: bool,
    /// The shielded actions already committed to the template.
    used: ShieldedActionCounts,
}

impl ShieldedBudget {
    /// Returns the budget for a block at `height` on `net`.
    fn new(net: &Network, height: Height) -> Self {
        Self {
            applies: NetworkUpgrade::current(net, height) >= NetworkUpgrade::Nu7,
            used: ShieldedActionCounts::default(),
        }
    }

    /// Adds `transaction` to the budget and returns `true`, or returns `false` and leaves the
    /// budget unchanged if it would exceed a limit.
    fn try_add(&mut self, transaction: &Transaction) -> bool {
        if !self.applies {
            return true;
        }

        let used = self
            .used
            .saturating_add(ShieldedActionCounts::from_transaction(transaction));

        if used.exceeded_limit().is_some() {
            return false;
        }

        self.used = used;
        true
    }
}

trait TryUpdateBlockLimits {
    /// Checks if a transaction fits within the provided remaining block bytes,
    /// sigops, and unpaid actions limits.
    ///
    /// Updates the limits and returns true if the transaction does fit, or
    /// returns false otherwise.
    fn try_update_block_template_limits(
        &self,
        remaining_block_bytes: &mut usize,
        remaining_block_sigops: &mut u32,
        remaining_block_unpaid_actions: &mut u32,
        remaining_shielded: &mut ShieldedBudget,
    ) -> bool;
}

impl TryUpdateBlockLimits for VerifiedUnminedTx {
    fn try_update_block_template_limits(
        &self,
        remaining_block_bytes: &mut usize,
        remaining_block_sigops: &mut u32,
        remaining_block_unpaid_actions: &mut u32,
        remaining_shielded: &mut ShieldedBudget,
    ) -> bool {
        // > If the block template with this transaction included
        // > would be within the block size limit and block sigop limit,
        // > and block_unpaid_actions <=  block_unpaid_action_limit,
        // > add the transaction to the block template
        //
        // Unpaid actions are always zero for transactions that pay the conventional fee, so the
        // unpaid action check always passes for those transactions. Use the full block-level sigop
        // count (legacy + P2SH) so template selection cannot produce blocks that the block verifier
        // would reject for exceeding `MAX_BLOCK_SIGOPS`.
        let tx_block_sigops = self.block_sigop_count();
        if self.transaction.size <= *remaining_block_bytes
            && tx_block_sigops <= *remaining_block_sigops
            && self.unpaid_actions <= *remaining_block_unpaid_actions
            // Checked last, and only committed once the other limits pass, so the shielded
            // budget is not consumed by a transaction that is then rejected for its size.
            && remaining_shielded.try_add(&self.transaction.transaction)
        {
            *remaining_block_bytes -= self.transaction.size;
            *remaining_block_sigops -= tx_block_sigops;

            // Unpaid actions are always zero for transactions that pay the conventional fee,
            // so this limit always remains the same after they are added.
            *remaining_block_unpaid_actions -= self.unpaid_actions;

            true
        } else {
            false
        }
    }
}
