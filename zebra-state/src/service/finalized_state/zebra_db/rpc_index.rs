//! The separate "RPC index" database: RPC-only transparent address / balance /
//! spent-tx indexes, written by a thread that trails the consensus database.
//!
//! This module provides the durable tip marker and the per-block index write for
//! the RPC index database, used only when
//! [`Config::separate_rpc_index_db`](crate::Config::separate_rpc_index_db) is set.
//!
//! See `docs/design/state-write-split.md` for the design, the crash-safety
//! analysis, and the column-family classification.
//!
//! # Correctness
//!
//! The RPC index database always **trails** the consensus database: the trailing
//! indexer only indexes blocks the consensus database has already made durable,
//! and it writes each block's index plus the advanced tip marker in one atomic
//! RPC-index-DB batch. So [`rpc_index_tip`](ZebraDb::rpc_index_tip) is never
//! ahead of the consensus finalized tip, and a crash leaves the RPC index at a
//! whole-block boundary that catch-up resumes from.

use std::collections::{BTreeMap, HashMap, HashSet};

use itertools::Itertools;

use zebra_chain::{
    block::{self, Height},
    parameters::Network,
    transaction, transparent,
};

use crate::{
    request::FinalizedBlock,
    service::finalized_state::{
        disk_db::{DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::transparent::{AddressBalanceLocationUpdates, OutputLocation},
        zebra_db::{transparent::TransparentBatchKind, ZebraDb},
        TransactionLocation, RPC_INDEX_TIP,
    },
};

impl ZebraDb {
    // RPC index tip marker

    /// Returns the height and hash of the highest block indexed into this RPC
    /// index database's RPC-only column families, if any.
    ///
    /// This is the durable tip marker stored in the [`RPC_INDEX_TIP`] column
    /// family, present only in a separate RPC index database. Returns `None`
    /// before any block has been indexed (an empty RPC index database).
    pub fn rpc_index_tip(&self) -> Option<(Height, block::Hash)> {
        let cf = self.db.cf_handle(RPC_INDEX_TIP)?;
        // A single-key column family: the highest (and only) entry is the tip.
        self.db.zs_last_key_value::<_, Height, block::Hash>(&cf)
    }

    /// Returns the height of the highest block indexed into this RPC index
    /// database, if any.
    pub fn rpc_index_tip_height(&self) -> Option<Height> {
        self.rpc_index_tip().map(|(height, _hash)| height)
    }

    /// Indexes `finalized` into this RPC index database, reading spend
    /// resolution and prior block / transaction data from `consensus_db`, and
    /// reading prior balances from this RPC index database, advancing the
    /// durable tip marker — all in one atomic batch.
    ///
    /// `self` must be the separate RPC index database (it owns the four RPC-only
    /// column families and the tip marker); `consensus_db` must be the consensus
    /// database that already holds `finalized` durably.
    ///
    /// `finalized` must be the block at `rpc_index_tip_height() + 1` (or genesis
    /// for an empty RPC index database); callers index blocks in strict height
    /// order, either replaying from the consensus database on catch-up or
    /// consuming the trailing channel in commit order.
    ///
    /// This reproduces exactly the RPC-only writes that the single-database
    /// `write_block` performs inline, so the resulting bytes are identical to
    /// the single-database path once the indexer catches up.
    ///
    /// The genesis block has no transparent index (see `write_block`), so only
    /// the tip marker is advanced for it.
    pub fn write_rpc_index_block(
        &self,
        consensus_db: &ZebraDb,
        finalized: &FinalizedBlock,
        network: &Network,
    ) -> Result<(), rocksdb::Error> {
        let mut batch = DiskWriteBatch::new();

        if !finalized.height.is_min() {
            self.prepare_rpc_index_transparent_batch(&mut batch, consensus_db, finalized, network);
        }

        // Advance the durable tip marker in the same batch, so the RPC index is
        // only ever durable at a whole-block boundary.
        self.prepare_rpc_index_tip(&mut batch, finalized.height, finalized.hash);

        self.db.write(batch)
    }

    /// Adds the RPC-index tip marker update for `(height, hash)` to `batch`.
    ///
    /// The tip column family holds at most one entry; the previous tip is
    /// deleted so a range scan or `zs_last_key_value` always returns the single
    /// current tip.
    fn prepare_rpc_index_tip(&self, batch: &mut DiskWriteBatch, height: Height, hash: block::Hash) {
        let cf = self
            .db
            .cf_handle(RPC_INDEX_TIP)
            .expect("rpc_index_tip column family exists in the RPC index database");

        if let Some((prev_height, _prev_hash)) = self.rpc_index_tip() {
            if prev_height != height {
                batch.zs_delete(&cf, prev_height);
            }
        }
        batch.zs_insert(&cf, height, hash);
    }

    /// Builds the RPC-only transparent index batch for `finalized`, reusing the
    /// same `prepare_transparent_transaction_batch` logic as the inline
    /// single-database path.
    ///
    /// Spent UTXOs are resolved from `consensus_db` (which already holds the
    /// block durably); prior balances are read from `self` (the RPC index
    /// database). The writes target the four RPC-only column families plus the
    /// balance column family, all of which live in `self`.
    fn prepare_rpc_index_transparent_batch(
        &self,
        batch: &mut DiskWriteBatch,
        consensus_db: &ZebraDb,
        finalized: &FinalizedBlock,
        network: &Network,
    ) {
        // The created outputs of this block, keyed by output location. The same
        // construction as `write_block`.
        let tx_hash_indexes: HashMap<transaction::Hash, usize> = finalized
            .transaction_hashes
            .iter()
            .enumerate()
            .map(|(index, hash)| (*hash, index))
            .collect();

        let new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = finalized
            .new_outputs
            .iter()
            .map(|(outpoint, ordered_utxo)| {
                let tx_index = tx_hash_indexes
                    .get(&outpoint.hash)
                    .expect("new output's transaction is in this block");
                let tx_loc = TransactionLocation::from_usize(finalized.height, *tx_index);
                (
                    OutputLocation::from_outpoint(tx_loc, outpoint),
                    ordered_utxo.utxo.clone(),
                )
            })
            .collect();

        // Resolve every spent output of this block from the consensus database.
        // The block is already durable there, so its inputs' outpoints all
        // resolve to stored output locations and output values.
        //
        // The spent output's value is read from the transaction body via
        // `output_by_location` (which reads `tx_by_loc`), NOT from
        // `utxo_by_location` (which reads `utxo_by_out_loc`). The consensus
        // commit deletes spent outputs from `utxo_by_out_loc`, so a cross-block
        // spend (an output created in an earlier block and spent here) is already
        // gone from the unspent set by the time this trailing indexer runs; the
        // transaction body is immutable, so it always resolves. Reading the
        // unspent set instead would silently drop every cross-block spend, making
        // the split RPC balances/UTXOs/tx-ids diverge from the single-database
        // path. See `docs/design/state-write-split.md`.
        let mut spent_utxos_by_outpoint: HashMap<transparent::OutPoint, transparent::Utxo> =
            HashMap::new();
        let mut out_loc_by_outpoint: HashMap<transparent::OutPoint, OutputLocation> =
            HashMap::new();
        let mut spent_utxos_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> =
            BTreeMap::new();

        for outpoint in finalized
            .block
            .transactions
            .iter()
            .flat_map(|tx| tx.inputs().iter())
            .flat_map(|input| input.outpoint())
        {
            let Some(out_loc) = consensus_db.output_location(&outpoint) else {
                // The output is not in the consensus database (e.g. it was
                // created in this same block and spent in it — already covered
                // by new_outputs — or genesis). Skip resolution; the address
                // passes tolerate missing spends the same way the inline path's
                // resolved set does.
                continue;
            };
            let Some(output) = consensus_db.output_by_location(&out_loc) else {
                continue;
            };
            // The trailing indexer never reads `Utxo::coinbase`/`height` to build
            // the transparent index (only the output's value and address), so
            // reconstruct the UTXO from the output location's height; `coinbase`
            // does not affect the index bytes.
            let utxo = transparent::Utxo::new(output, out_loc.height(), false);
            spent_utxos_by_outpoint.insert(outpoint, utxo.clone());
            out_loc_by_outpoint.insert(outpoint, out_loc);
            spent_utxos_by_out_loc.insert(out_loc, utxo);
        }

        // The transparent addresses with changed balances/UTXOs.
        let changed_addresses: HashSet<transparent::Address> = spent_utxos_by_out_loc
            .values()
            .chain(
                finalized
                    .new_outputs
                    .values()
                    .map(|ordered_utxo| &ordered_utxo.utxo),
            )
            .filter_map(|utxo| utxo.output.address(network))
            .unique()
            .collect();

        // Read the prior balances from the RPC index database (self), exactly
        // as the inline path reads them from the single database. The RPC index
        // database always has finished format upgrades (it is created fresh by
        // the split), so use plain inserts.
        let address_balances = AddressBalanceLocationUpdates::Insert(
            changed_addresses
                .into_iter()
                .filter_map(|address| Some((address, self.address_balance_location(&address)?)))
                .collect(),
        );

        batch.prepare_transparent_transaction_batch_split(
            self,
            network,
            finalized,
            &new_outputs_by_out_loc,
            &spent_utxos_by_outpoint,
            &spent_utxos_by_out_loc,
            &out_loc_by_outpoint,
            address_balances,
            TransparentBatchKind::RpcOnly,
        );
    }
}
