//! The checks of an imported state snapshot (design, "Trust model and
//! verification"): against the chain,
//! through the history tree, the checkpoints and the window blocks, and
//! against itself, through its indexes and value pools.
//!
//! [`check_raw`] runs on the database the importer has just written, before
//! it is closed; [`check_database`] reopens it the way the node will and runs
//! the database format checks. Both run before the database is renamed into
//! place. Every record they read has passed its record check, so the
//! database's `FromDisk` readers can decode it.

use std::collections::{BTreeMap, HashSet};

use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode, DB};

use zebra_chain::{
    amount::NonNegative,
    block::{self, Height},
    history_tree::NonEmptyHistoryTree,
    orchard,
    parameters::{Network, NetworkUpgrade, GENESIS_PREVIOUS_BLOCK_HASH},
    sapling, sprout,
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{
        CancelFormatChange, DbFormatChange, FormatCheckError, TransactionLocation, ZebraDb,
        OUTPUT_LOCATION_DISK_BYTES, TRANSACTION_LOCATION_DISK_BYTES,
    },
    state_snapshot::format::{ComponentId, Manifest},
    Config, FromDisk, IntoDisk,
};

use super::{
    column_family, db_error, records, refused, ImportCheck, Singletons, SnapshotBase, SnapshotError,
};

/// Decodes and checks the history tree at the snapshot height: it is for this
/// network, its last block is the snapshot block, and its rightmost peak ends
/// there (design, "Trust model and verification").
pub(super) fn history_tree(
    bytes: &[u8],
    base: &SnapshotBase,
    network: &Network,
) -> Result<NonEmptyHistoryTree, SnapshotError> {
    let parts = records::history_tree_parts(bytes)
        .map_err(|detail| refused(ImportCheck::Records, detail))?;
    let fail = |detail: String| refused(ImportCheck::HistoryTree, detail);
    if parts.network_kind() != network.kind() {
        return Err(fail(format!(
            "the history tree is for {:?}, not {:?}",
            parts.network_kind(),
            network.kind()
        )));
    }
    if parts.current_height() != base.height {
        return Err(fail(format!(
            "the history tree ends at {:?}, not the snapshot height {:?}",
            parts.current_height(),
            base.height
        )));
    }
    if parts.peak_count() == 0 {
        return Err(fail("the history tree has no peaks".to_string()));
    }
    records::check_history_tree_peaks(bytes, NetworkUpgrade::current(network, base.height))
        .map_err(fail)?;

    // The tree's network upgrade is the one at its current height, which is the snapshot height,
    // where NU5 is active, so the tree has a version, and it is `current(H)`.
    let tree = parts
        .with_network(network)
        .map_err(|error| fail(format!("invalid history tree: {error}")))?;
    let peak = tree
        .last_peak()
        .map_err(|error| fail(format!("invalid history tree peak: {error}")))?;
    if peak.end_height != base.height {
        return Err(fail(format!(
            "the rightmost history tree peak ends at {:?}, not the snapshot height {:?}",
            peak.end_height, base.height
        )));
    }
    Ok(tree)
}

/// Runs the checks that read the database the importer has just written.
pub(super) fn check_raw(
    db: &DB,
    manifest: &Manifest,
    base: &SnapshotBase,
    network: &Network,
    singletons: &Singletons,
) -> Result<(), SnapshotError> {
    let raw = Raw { db, manifest };
    check_window(&raw, base, network)?;
    check_tree_roots(&raw, base, network, singletons)?;
    check_transaction_index(&raw, base)?;
    let transparent_total = check_address_index(&raw, base, network)?;
    check_value_pools(&raw, base, singletons, transparent_total)
}

/// Read access to the database the importer has just written.
struct Raw<'db> {
    db: &'db DB,
    manifest: &'db Manifest,
}

impl<'db> Raw<'db> {
    /// Returns the column family of `component`.
    fn cf(&self, component: ComponentId) -> &'db ColumnFamily {
        column_family(self.db, component.column_family())
    }

    /// Returns the value of `key` in `component`.
    fn get(&self, component: ComponentId, key: &[u8]) -> Result<Option<Vec<u8>>, SnapshotError> {
        self.db
            .get_cf(self.cf(component), key)
            .map_err(db_error("reading the import database"))
    }

    /// Returns whether `component` has a record at `key`.
    fn contains(&self, component: ComponentId, key: &[u8]) -> Result<bool, SnapshotError> {
        Ok(self
            .db
            .get_pinned_cf(self.cf(component), key)
            .map_err(db_error("reading the import database"))?
            .is_some())
    }

    /// Returns an iterator over `component` from the first key at or above
    /// `from`.
    fn iter_from(
        &self,
        component: ComponentId,
        from: &[u8],
    ) -> DBRawIteratorWithThreadMode<'db, DB> {
        let mut iterator = self.db.raw_iterator_cf(self.cf(component));
        iterator.seek(from);
        iterator
    }

    /// Returns the number of records the manifest gives `component`, which the
    /// importer checked against the pieces.
    fn entries(&self, component: ComponentId) -> u64 {
        self.manifest.component(component).entry_count
    }
}

/// Returns the item at a valid raw iterator.
fn item<'a>(iterator: &'a DBRawIteratorWithThreadMode<'_, DB>) -> (&'a [u8], &'a [u8]) {
    iterator
        .item()
        .expect("the caller checked that the iterator is valid")
}

/// Returns an error if an iteration stopped on a database error.
fn finished(iterator: &DBRawIteratorWithThreadMode<'_, DB>) -> Result<(), SnapshotError> {
    iterator
        .status()
        .map_err(db_error("iterating the import database"))
}

/// Returns the heights a snapshot carries blocks for: genesis, then the
/// window `L..=H`.
fn block_heights(base: &SnapshotBase) -> impl Iterator<Item = Height> {
    std::iter::once(Height(0)).chain((base.window_start.0..=base.height.0).map(Height))
}

/// Returns the number of heights a snapshot carries blocks for.
fn block_count(base: &SnapshotBase) -> u64 {
    // `check_manifest` checked that the window starts at or below the snapshot height.
    u64::from(base.height.0 - base.window_start.0) + 2
}

/// Checks the genesis and window blocks (design, "Trust model and
/// verification"): hashes and heights
/// index exactly those blocks, the window headers chain to the snapshot block,
/// the genesis block is the network's, every body's transactions match their
/// index rows and their header's merkle root, and the block info covers the
/// window and its parent.
fn check_window(raw: &Raw, base: &SnapshotBase, network: &Network) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::Window, detail);

    let mut hashes = Vec::new();
    let mut expected = block_heights(base);
    let mut iterator = raw.iter_from(ComponentId::HashByHeight, &[]);
    while iterator.valid() {
        let (key, value) = item(&iterator);
        let height = records::height(key);
        match expected.next() {
            Some(next) if next == height => hashes.push((height, block::Hash::from_bytes(value))),
            Some(next) => {
                return Err(fail(format!(
                    "expected the block hash at {next:?}, found one at {height:?}"
                )))
            }
            None => return Err(fail(format!("unexpected block hash at {height:?}"))),
        }
        iterator.next();
    }
    finished(&iterator)?;
    if let Some(next) = expected.next() {
        return Err(fail(format!("the block hash at {next:?} is missing")));
    }
    if hashes.first().map(|(_, hash)| *hash) != Some(network.genesis_hash()) {
        return Err(fail(
            "the genesis block hash is not the network's".to_string(),
        ));
    }
    if hashes.last() != Some(&(base.height, base.block_hash)) {
        return Err(fail(
            "the block hash at the snapshot height is not the snapshot block".to_string(),
        ));
    }

    for component in [ComponentId::HeightByHash, ComponentId::Headers] {
        if raw.entries(component) != block_count(base) {
            return Err(fail(format!(
                "{component} has {} records, not one per block",
                raw.entries(component)
            )));
        }
    }

    let mut previous_hash = None;
    let mut transactions = 0u64;
    let mut recipients = HashSet::new();
    for (height, hash) in &hashes {
        if raw.get(ComponentId::HeightByHash, &hash.as_bytes())? != Some(height.as_bytes().to_vec())
        {
            return Err(fail(format!(
                "the height index of block {hash} is not {height:?}"
            )));
        }
        let header = raw
            .get(ComponentId::Headers, &height.as_bytes())?
            .map(block::Header::from_bytes)
            .ok_or_else(|| fail(format!("the header at {height:?} is missing")))?;
        if header.hash() != *hash {
            return Err(fail(format!(
                "the header at {height:?} is not block {hash}"
            )));
        }
        let expected_previous = if height.is_min() {
            Some(GENESIS_PREVIOUS_BLOCK_HASH)
        } else {
            previous_hash
        };
        if let Some(expected_previous) = expected_previous {
            if header.previous_block_hash != expected_previous {
                return Err(fail(format!(
                    "the header at {height:?} does not follow block {expected_previous}"
                )));
            }
        }
        // Block `L - 1` is not in the snapshot unless it is genesis.
        previous_hash = if height.is_min() && base.window_start != Height(1) {
            None
        } else {
            Some(*hash)
        };

        let txids = check_body(raw, *height, network, &mut recipients)?;
        let merkle_root: block::merkle::Root = txids.iter().copied().collect();
        if merkle_root != header.merkle_root {
            return Err(fail(format!(
                "the transactions at {height:?} do not have the header's merkle root"
            )));
        }
        // `txids` holds the transactions of one stored block.
        transactions = transactions.saturating_add(txids.len() as u64);
    }
    if transactions != raw.entries(ComponentId::Transactions) {
        return Err(fail(format!(
            "{} has {} records, but the blocks have {transactions} transactions",
            ComponentId::Transactions,
            raw.entries(ComponentId::Transactions)
        )));
    }

    for address in recipients {
        let received = raw
            .get(ComponentId::AddressBalances, &address.as_bytes())?
            .map(|balance| records::address_balance(&balance))
            .transpose()
            .map_err(|detail| refused(ImportCheck::Records, detail))?
            .map(|balance| balance.received);
        if received.is_none_or(|received| received == 0) {
            return Err(refused(
                ImportCheck::AddressIndex,
                format!("{address} received an output in the window, but has no balance"),
            ));
        }
    }

    check_block_info(raw, base)
}

/// Checks the body of the block at `height`: its transactions are contiguous
/// from the coinbase and each has its hash and location index rows, and only
/// those. Returns the transaction hashes in block order, and adds the
/// addresses that received a non-zero output to `recipients`.
fn check_body(
    raw: &Raw,
    height: Height,
    network: &Network,
    recipients: &mut HashSet<transparent::Address>,
) -> Result<Vec<transaction::Hash>, SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::Window, detail);
    let last = TransactionLocation::max_for_height(height).as_bytes();

    let mut txids = Vec::new();
    let mut iterator = raw.iter_from(
        ComponentId::Transactions,
        &TransactionLocation::min_for_height(height).as_bytes(),
    );
    while iterator.valid() && item(&iterator).0 <= last.as_slice() {
        let (key, value) = item(&iterator);
        let location = records::transaction_location(key);
        if location.index.as_usize() != txids.len() {
            return Err(fail(format!(
                "the body at {height:?} is missing transaction {}",
                txids.len()
            )));
        }
        let transaction = Transaction::from_bytes(value);
        // The address indexes skip genesis, whose outputs are never spendable, and so does the
        // database format check this protects.
        if !height.is_min() {
            recipients.extend(
                transaction
                    .outputs()
                    .iter()
                    .filter(|output| output.value != 0)
                    .filter_map(|output| output.address(network)),
            );
        }
        let txid = transaction.hash();
        if raw.get(ComponentId::HashByTxLoc, key)? != Some(txid.as_bytes().to_vec()) {
            return Err(fail(format!(
                "transaction {txid} at {location:?} has no hash index row"
            )));
        }
        if raw.get(ComponentId::TxLocByHash, &txid.as_bytes())? != Some(key.to_vec()) {
            return Err(fail(format!(
                "transaction {txid} at {location:?} has no location index row"
            )));
        }
        txids.push(txid);
        iterator.next();
    }
    finished(&iterator)?;
    if txids.is_empty() {
        return Err(fail(format!("the body at {height:?} is missing")));
    }

    let mut index_rows = 0usize;
    let mut iterator = raw.iter_from(
        ComponentId::HashByTxLoc,
        &TransactionLocation::min_for_height(height).as_bytes(),
    );
    while iterator.valid() && item(&iterator).0 <= last.as_slice() {
        index_rows = index_rows.saturating_add(1);
        iterator.next();
    }
    finished(&iterator)?;
    if index_rows != txids.len() {
        return Err(fail(format!(
            "the block at {height:?} has {} transactions but {index_rows} hash index rows",
            txids.len()
        )));
    }

    Ok(txids)
}

/// Checks that the block info rows are exactly those of the window, its
/// parent and genesis, and that none of them is empty.
fn check_block_info(raw: &Raw, base: &SnapshotBase) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::Window, detail);
    // `check_manifest` checked that the window starts above genesis.
    let first = Height(base.window_start.0 - 1);

    let mut window_rows = 0u64;
    let mut iterator = raw.iter_from(ComponentId::BlockInfo, &[]);
    while iterator.valid() {
        let (key, value) = item(&iterator);
        let height = records::height(key);
        if !(height.is_min() || (first..=base.height).contains(&height)) {
            return Err(fail(format!("unexpected block info at {height:?}")));
        }
        if zebra_chain::block_info::BlockInfo::from_bytes(value) == Default::default() {
            return Err(fail(format!("the block info at {height:?} is empty")));
        }
        if height >= first {
            window_rows = window_rows.saturating_add(1);
        }
        iterator.next();
    }
    finished(&iterator)?;
    if window_rows != u64::from(base.height.0 - first.0) + 1 {
        return Err(fail(format!(
            "the block info from {first:?} to the snapshot height is incomplete"
        )));
    }
    Ok(())
}

/// Returns the last record of the tree `component` at or below `height`, after
/// checking that the tree has no record above it and one at genesis.
fn tree_at(raw: &Raw, component: ComponentId, height: Height) -> Result<Vec<u8>, SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::TreeRoots, detail);

    let mut iterator = raw.db.raw_iterator_cf(raw.cf(component));
    iterator.seek_to_last();
    if iterator.valid() && records::height(item(&iterator).0) > height {
        return Err(fail(format!(
            "{component} has a tree above the snapshot height"
        )));
    }
    finished(&iterator)?;
    if !raw.contains(component, &Height(0).as_bytes())? {
        return Err(fail(format!("{component} has no genesis tree")));
    }

    iterator.seek_for_prev(height.as_bytes());
    let tree = iterator.valid().then(|| item(&iterator).1.to_vec());
    finished(&iterator)?;
    tree.ok_or_else(|| fail(format!("{component} has no tree at the snapshot height")))
}

/// Checks the note commitment trees at the snapshot height (design, "Trust
/// model and verification"):
/// their roots, recomputed from their frontiers, are the roots the history
/// tree's rightmost peak commits to, and anchors, like the empty genesis
/// roots; the Ironwood state is empty before NU6.3.
fn check_tree_roots(
    raw: &Raw,
    base: &SnapshotBase,
    network: &Network,
    singletons: &Singletons,
) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::TreeRoots, detail);
    let records_error = |detail| refused(ImportCheck::Records, detail);
    let peak = singletons
        .history_tree
        .last_peak()
        .map_err(|error| refused(ImportCheck::HistoryTree, error.to_string()))?;
    let anchor = |component, root: [u8; 32], what: &str| -> Result<(), SnapshotError> {
        if raw.contains(component, &root)? {
            Ok(())
        } else {
            Err(fail(format!("the {what} root is not an anchor")))
        }
    };

    let sapling = records::sapling_tree(&tree_at(raw, ComponentId::SaplingTree, base.height)?)
        .map_err(records_error)?
        .recalculate_root();
    if <[u8; 32]>::from(sapling) != peak.end_sapling_root {
        return Err(fail(
            "the Sapling tree root is not the history tree's".to_string(),
        ));
    }
    anchor(ComponentId::SaplingAnchors, sapling.into(), "Sapling tip")?;

    let orchard = records::orchard_tree(&tree_at(raw, ComponentId::OrchardTree, base.height)?)
        .map_err(records_error)?
        .recalculate_root();
    if Some(<[u8; 32]>::from(orchard)) != peak.end_orchard_root {
        return Err(fail(
            "the Orchard tree root is not the history tree's".to_string(),
        ));
    }
    anchor(ComponentId::OrchardAnchors, orchard.into(), "Orchard tip")?;

    let ironwood_tree =
        records::orchard_tree(&tree_at(raw, ComponentId::IronwoodTree, base.height)?)
            .map_err(records_error)?;
    let ironwood = ironwood_tree.recalculate_root();
    let ironwood_active = NetworkUpgrade::Nu6_3
        .activation_height(network)
        .is_some_and(|activation| activation <= base.height);
    if ironwood_active {
        if Some(<[u8; 32]>::from(ironwood)) != peak.end_ironwood_root {
            return Err(fail(
                "the Ironwood tree root is not the history tree's".to_string(),
            ));
        }
    } else if ironwood_tree.count() != 0
        || peak.end_ironwood_root.is_some()
        || raw.entries(ComponentId::IronwoodNullifiers) != 0
        || raw.entries(ComponentId::IronwoodSubtrees) != 0
        || raw.entries(ComponentId::IronwoodAnchors) != 1
    {
        return Err(fail(
            "the Ironwood state is not empty before NU6.3".to_string(),
        ));
    }
    anchor(
        ComponentId::IronwoodAnchors,
        ironwood.into(),
        "Ironwood tip",
    )?;

    let sprout = singletons.sprout_tree.recalculate_root();
    anchor(ComponentId::SproutAnchors, sprout.into(), "Sprout tip")?;
    anchor(
        ComponentId::SproutAnchors,
        sprout::tree::NoteCommitmentTree::default()
            .recalculate_root()
            .into(),
        "empty Sprout",
    )?;
    anchor(
        ComponentId::SaplingAnchors,
        sapling::tree::NoteCommitmentTree::default()
            .recalculate_root()
            .into(),
        "empty Sapling",
    )?;
    let empty_orchard: [u8; 32] = orchard::tree::NoteCommitmentTree::default()
        .recalculate_root()
        .into();
    anchor(ComponentId::OrchardAnchors, empty_orchard, "empty Orchard")?;
    anchor(
        ComponentId::IronwoodAnchors,
        empty_orchard,
        "empty Ironwood",
    )
}

/// Checks that every transaction location below the window has a location
/// index row that points back at it, so the two transaction indexes are each
/// other's inverse, and that every address transaction row is for a
/// transaction with a hash index row. [`check_window`] checks the rows of the
/// stored blocks.
fn check_transaction_index(raw: &Raw, base: &SnapshotBase) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::TransactionIndex, detail);

    let mut iterator = raw.iter_from(ComponentId::HashByTxLoc, &[]);
    while iterator.valid() {
        let (key, hash) = item(&iterator);
        let location = records::transaction_location(key);
        if location.height > base.height {
            return Err(fail(format!(
                "transaction location {location:?} is above the snapshot height"
            )));
        }
        let stored = location.height.is_min() || location.height >= base.window_start;
        if !stored && raw.get(ComponentId::TxLocByHash, hash)?.as_deref() != Some(key) {
            return Err(fail(format!(
                "the location index of transaction {} does not point at {location:?}",
                transaction::Hash::from_bytes(hash)
            )));
        }
        iterator.next();
    }
    finished(&iterator)?;

    if raw.entries(ComponentId::TxLocByHash) != raw.entries(ComponentId::HashByTxLoc) {
        return Err(fail(format!(
            "{} has {} records, but {} has {}",
            ComponentId::TxLocByHash,
            raw.entries(ComponentId::TxLocByHash),
            ComponentId::HashByTxLoc,
            raw.entries(ComponentId::HashByTxLoc),
        )));
    }

    // The address history reads the hash of every transaction an address row names.
    let mut iterator = raw.iter_from(ComponentId::AddressTxs, &[]);
    while iterator.valid() {
        let (key, _) = item(&iterator);
        let transaction = &key[OUTPUT_LOCATION_DISK_BYTES..];
        if !raw.contains(ComponentId::HashByTxLoc, transaction)? {
            return Err(fail(format!(
                "an address transaction row names {:?}, which has no hash index row",
                records::transaction_location(transaction)
            )));
        }
        iterator.next();
    }
    finished(&iterator)
}

/// Checks the UTXO set against the transaction and address indexes (design,
/// "Trust model and verification"), and returns the total value of the UTXO set.
///
/// Every UTXO's transaction has a hash index row; every address UTXO row
/// points at a UTXO with an address whose balance names the row's first
/// output, so the rows are exactly those of the UTXOs with an address when
/// there are as many rows as such UTXOs; each address's balance is the sum of
/// its UTXOs; and the addresses without UTXOs have no balance.
fn check_address_index(
    raw: &Raw,
    base: &SnapshotBase,
    network: &Network,
) -> Result<u64, SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::AddressIndex, detail);
    let overflow = || fail("the UTXO values overflow".to_string());

    // The UTXO keys start with their transaction's location, which keys the hash index, so a
    // second iterator walks the index alongside the UTXOs instead of reading it per UTXO.
    let mut total = 0u64;
    let mut address_total = 0u64;
    let mut address_utxos = 0u64;
    let mut transactions = raw.iter_from(ComponentId::HashByTxLoc, &[]);
    let mut iterator = raw.iter_from(ComponentId::Utxos, &[]);
    while iterator.valid() {
        let (key, value) = item(&iterator);
        let location = records::output_location(key);
        if location.height().is_min() || location.height() > base.height {
            return Err(fail(format!(
                "unspent output {location:?} is not from a block between genesis and the \
                 snapshot height"
            )));
        }
        let transaction = &key[..TRANSACTION_LOCATION_DISK_BYTES];
        while transactions.valid() && item(&transactions).0 < transaction {
            transactions.next();
        }
        if !(transactions.valid() && item(&transactions).0 == transaction) {
            finished(&transactions)?;
            return Err(refused(
                ImportCheck::TransactionIndex,
                format!("unspent output {location:?} has no transaction hash index row"),
            ));
        }

        let output = transparent::Output::from_bytes(value);
        total = total
            .checked_add(u64::from(output.value))
            .ok_or_else(overflow)?;
        if output.address(network).is_some() {
            address_total = address_total
                .checked_add(u64::from(output.value))
                .ok_or_else(overflow)?;
            address_utxos = address_utxos.saturating_add(1);
        }
        iterator.next();
    }
    finished(&iterator)?;
    finished(&transactions)?;

    // Each address UTXO row is checked below to point at a distinct UTXO with an address, so if
    // there are as many rows as such UTXOs, the rows are exactly theirs.
    if raw.entries(ComponentId::AddressUtxos) != address_utxos {
        return Err(fail(format!(
            "{} has {} records, but {address_utxos} unspent outputs have an address",
            ComponentId::AddressUtxos,
            raw.entries(ComponentId::AddressUtxos)
        )));
    }

    // The address UTXO rows are keyed by the address's first output, then the unspent output, so
    // each address's UTXOs are contiguous. Sum them per address, in case a snapshot gives two
    // addresses the same first output.
    let mut group: Option<(Vec<u8>, BTreeMap<[u8; 21], u64>)> = None;
    let mut iterator = raw.iter_from(ComponentId::AddressUtxos, &[]);
    while iterator.valid() {
        let (key, _) = item(&iterator);
        let (address_location, output_location) = key.split_at(key.len() / 2);
        if group
            .as_ref()
            .is_some_and(|(location, _)| location.as_slice() != address_location)
        {
            let (location, sums) = group.take().expect("checked just above");
            check_balances(raw, &location, sums)?;
        }
        let output = raw
            .get(ComponentId::Utxos, output_location)?
            .map(transparent::Output::from_bytes)
            .ok_or_else(|| fail("an address UTXO row points at a spent output".to_string()))?;
        let address = output.address(network).ok_or_else(|| {
            fail("an address UTXO row points at an output without an address".to_string())
        })?;
        let (_, sums) = group.get_or_insert_with(|| (address_location.to_vec(), BTreeMap::new()));
        let sum = sums.entry(address.as_bytes()).or_default();
        *sum = sum
            .checked_add(u64::from(output.value))
            .ok_or_else(overflow)?;
        iterator.next();
    }
    finished(&iterator)?;
    if let Some((location, sums)) = group {
        check_balances(raw, &location, sums)?;
    }

    let mut balances = 0u64;
    let mut iterator = raw.iter_from(ComponentId::AddressBalances, &[]);
    while iterator.valid() {
        let balance = records::address_balance(item(&iterator).1)
            .map_err(|detail| refused(ImportCheck::Records, detail))?;
        balances = balances
            .checked_add(u64::from(balance.balance))
            .ok_or_else(overflow)?;
        iterator.next();
    }
    finished(&iterator)?;
    // Each address with UTXOs has their sum as its balance, so the others sum to zero, and
    // balances are never negative.
    if balances != address_total {
        return Err(fail(format!(
            "the address balances sum to {balances}, but the UTXOs with an address to \
             {address_total}"
        )));
    }

    Ok(total)
}

/// Checks that each address in `sums` has a balance, with its sum as the
/// balance, and `location` as its first output.
fn check_balances(
    raw: &Raw,
    location: &[u8],
    sums: BTreeMap<[u8; 21], u64>,
) -> Result<(), SnapshotError> {
    for (address, sum) in sums {
        let balance = raw
            .get(ComponentId::AddressBalances, &address)?
            .map(|balance| records::address_balance(&balance))
            .transpose()
            .map_err(|detail| refused(ImportCheck::Records, detail))?;
        if balance.is_none_or(|balance| {
            u64::from(balance.balance) != sum || balance.location.as_bytes() != location
        }) {
            return Err(refused(
                ImportCheck::AddressIndex,
                format!(
                    "the balance of address {} is not the sum of its {sum} zatoshis of UTXOs",
                    hex::encode(address)
                ),
            ));
        }
    }
    Ok(())
}

/// Checks the value pools at the snapshot height: they are the block info's
/// at that height, and the transparent pool is the value of the UTXO set.
fn check_value_pools(
    raw: &Raw,
    base: &SnapshotBase,
    singletons: &Singletons,
    transparent_total: u64,
) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::ValuePools, detail);
    let block_info = raw
        .get(ComponentId::BlockInfo, &base.height.as_bytes())?
        .map(zebra_chain::block_info::BlockInfo::from_bytes)
        .ok_or_else(|| fail("the block info at the snapshot height is missing".to_string()))?;
    let value_pools: ValueBalance<NonNegative> = singletons.value_pools;
    if *block_info.value_pools() != value_pools {
        return Err(fail(
            "the value pools are not the block info's at the snapshot height".to_string(),
        ));
    }

    // Neither the UTXO set nor the transparent pool has the genesis outputs, which are never
    // spendable, and every other transparent output is in the UTXO set until it is spent.
    let transparent_pool = u64::from(value_pools.transparent_amount());
    if transparent_pool != transparent_total {
        return Err(fail(format!(
            "the transparent pool is {transparent_pool} zatoshis, but the UTXO set holds \
             {transparent_total}"
        )));
    }
    Ok(())
}

/// Reopens the imported database the way the node will open it, and checks
/// that it opens as the current format, describes the snapshot, and passes the
/// database format checks that run at every start.
///
/// The checks run on this thread, so an invalid database is refused instead of
/// panicking the format check thread.
pub(super) fn check_database(
    config: &Config,
    network: &Network,
    base: &SnapshotBase,
) -> Result<(), SnapshotError> {
    let db = ZebraDb::open_without_format_change(config, network).map_err(SnapshotError::Open)?;
    check_open_database(&db, base)
}

/// Runs the post-open database checks on `db`.
pub(super) fn check_open_database(db: &ZebraDb, base: &SnapshotBase) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::DatabaseFormat, detail);
    let running_version = db.format_version_in_code();
    let disk_version = db
        .format_version_on_disk()
        .map_err(|source| SnapshotError::FormatVersion { source })?;
    if !matches!(
        DbFormatChange::open_database(&running_version, disk_version.clone()),
        DbFormatChange::CheckOpenCurrent { .. }
    ) {
        return Err(fail(format!(
            "the database is in format {disk_version:?}, not {running_version}"
        )));
    }

    let history = db.history_capabilities()?;
    if history.snapshot_base != Some(*base)
        || history.lowest_known_height != base.window_start
        || history.lowest_retained_height != base.window_start
    {
        return Err(fail(format!(
            "the database's history is {history:?}, not that of the snapshot {base:?}"
        )));
    }
    if db.tip() != Some((base.height, base.block_hash)) {
        return Err(fail(
            "the database's tip is not the snapshot block".to_string(),
        ));
    }
    match db.tip_block() {
        Ok(Some(block)) if block.hash() == base.block_hash => {}
        other => {
            return Err(fail(format!(
                "the database's tip block does not read back: {other:?}"
            )))
        }
    }

    // The checks are never cancelled: the sender lives until they return.
    let (_cancel, cancel_receiver) = crossbeam_channel::bounded(1);
    match DbFormatChange::format_validity_checks_detailed_typed(db, &cancel_receiver) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(database_format_error(error)),
        Err(CancelFormatChange) => Err(fail("the format checks were cancelled".to_string())),
    }
}

/// Maps invalid snapshot contents to a refusal, but keeps local history-read failures retryable.
pub(super) fn database_format_error(error: FormatCheckError) -> SnapshotError {
    match error {
        FormatCheckError::Invalid(detail) => refused(ImportCheck::DatabaseFormat, detail),
        FormatCheckError::HistoryRead(error) => SnapshotError::HistoryRead(error),
    }
}
