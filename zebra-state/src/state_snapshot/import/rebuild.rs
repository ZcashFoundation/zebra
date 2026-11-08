//! The indexes a state snapshot does not ship, rebuilt by the importer from
//! the components it does (design, "Snapshot contents").
//!
//! - `hash_by_tx_loc` is the inverse of the shipped `tx_loc_by_hash`.
//! - `balance_by_transparent_addr` and `utxo_loc_by_transparent_addr_loc`
//!   follow from the UTXO set: each address's balance is the sum of its
//!   unspent outputs, and its location, which keys its index rows, is its
//!   first unspent output. Its received total is unknown, because the outputs
//!   it spent below the snapshot height are gone, so it starts at zero.
//! - `tx_loc_by_transparent_addr_loc` is not rebuilt: an address's history
//!   below the snapshot height is gone with the bodies, so the index starts
//!   above it.
//!
//! The rebuilt rows are written as SST files and ingested like the pieces.

use std::{collections::HashMap, fs, path::Path};

use rocksdb::{SstFileWriter, DB};

use zebra_chain::{parameters::Network, transparent};

use crate::{
    service::finalized_state::{
        AddressBalanceLocation, AddressUnspentOutput, DiskDb, OutputLocation,
    },
    state_snapshot::format::ComponentId,
    FromDisk, IntoDisk, TransactionLocation,
};

use super::{column_family, db_error, io_error, records, refused, ImportCheck, SnapshotError};

/// The column family of the transaction hashes by location.
pub(super) const HASH_BY_TX_LOC: &str = "hash_by_tx_loc";

/// The column family of the address balances.
pub(super) const ADDRESS_BALANCES: &str = "balance_by_transparent_addr";

/// The column family of the address UTXO index.
pub(super) const ADDRESS_UTXOS: &str = "utxo_loc_by_transparent_addr_loc";

/// Rebuilds the indexes a snapshot does not ship into the import database
/// `db`, from its ingested components, writing the SST files under
/// `temp_root`.
///
/// Refuses a snapshot whose transaction locations name one location twice,
/// because their inverse would lose a transaction.
pub(super) fn rebuild_indexes(
    db: &DB,
    temp_root: &Path,
    network: &Network,
) -> Result<(), SnapshotError> {
    let directory = temp_root.join("rebuilt");
    fs::create_dir_all(&directory).map_err(io_error(&directory))?;

    rebuild_hash_by_tx_loc(db, &directory)?;
    rebuild_address_indexes(db, &directory, network)
}

/// Rebuilds `hash_by_tx_loc` as the inverse of `tx_loc_by_hash`.
fn rebuild_hash_by_tx_loc(db: &DB, directory: &Path) -> Result<(), SnapshotError> {
    let mut rows: Vec<(Vec<u8>, [u8; 32])> = Vec::new();
    let mut iterator =
        db.raw_iterator_cf(column_family(db, ComponentId::TxLocByHash.column_family()));
    iterator.seek_to_first();
    while let Some((hash, location)) = iterator.item() {
        // `check_record` checked the encodings of every shipped record, so a malformed row here
        // means the import database changed after the checks: refuse it instead of panicking.
        let malformed = || {
            refused(
                ImportCheck::Records,
                "a transaction location row is malformed",
            )
        };
        if !TransactionLocation::from_bytes_prefix(location)
            .is_some_and(|(_, rest)| rest.is_empty())
        {
            return Err(malformed());
        }
        rows.push((location.to_vec(), hash.try_into().map_err(|_| malformed())?));
        iterator.next();
    }
    iterator
        .status()
        .map_err(db_error("iterating the import database"))?;
    drop(iterator);

    rows.sort_unstable();
    if let Some(pair) = rows.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(refused(
            ImportCheck::TransactionIndex,
            format!(
                "two transactions are at {:?}",
                records::transaction_location(&pair[0].0)
            ),
        ));
    }

    write_and_ingest(
        db,
        &directory.join("hash_by_tx_loc.sst"),
        HASH_BY_TX_LOC,
        rows.iter()
            .map(|(location, hash)| (&location[..], &hash[..])),
    )
}

/// Rebuilds the address balances and the address UTXO index from the UTXO
/// set.
fn rebuild_address_indexes(
    db: &DB,
    directory: &Path,
    network: &Network,
) -> Result<(), SnapshotError> {
    let overflow = || refused(ImportCheck::AddressIndex, "an address balance overflows");

    let mut balances: HashMap<transparent::Address, AddressBalanceLocation> = HashMap::new();
    let mut utxos = Vec::new();
    let mut iterator = db.raw_iterator_cf(column_family(db, ComponentId::Utxos.column_family()));
    iterator.seek_to_first();
    while let Some((key, value)) = iterator.item() {
        let output = transparent::Output::from_bytes(value);
        if let Some(address) = output.address(network) {
            let location = OutputLocation::from_bytes(key);
            // The UTXO set is in location order, so an address's first UTXO is the first seen.
            let balance = balances
                .entry(address)
                .or_insert_with(|| AddressBalanceLocation::new(location));
            *balance.balance_mut() =
                (balance.balance() + output.value()).map_err(|_| overflow())?;
            utxos.push(AddressUnspentOutput::new(balance.address_location(), location).as_bytes());
        }
        iterator.next();
    }
    iterator
        .status()
        .map_err(db_error("iterating the import database"))?;
    drop(iterator);

    utxos.sort_unstable();
    write_and_ingest(
        db,
        &directory.join("utxo_loc_by_transparent_addr_loc.sst"),
        ADDRESS_UTXOS,
        utxos.iter().map(|key| (&key[..], &[][..])),
    )?;

    let mut balances: Vec<_> = balances
        .into_iter()
        .map(|(address, balance)| (address.as_bytes(), balance.as_bytes()))
        .collect();
    balances.sort_unstable();
    write_and_ingest(
        db,
        &directory.join("balance_by_transparent_addr.sst"),
        ADDRESS_BALANCES,
        balances
            .iter()
            .map(|(address, balance)| (&address[..], &balance[..])),
    )
}

/// Writes `rows`, which are in strictly increasing key order, to an SST file
/// at `path`, and ingests it into the empty column family `cf`. Writes nothing
/// when there are no rows.
fn write_and_ingest<'a>(
    db: &DB,
    path: &Path,
    cf: &str,
    rows: impl Iterator<Item = (&'a [u8], &'a [u8])>,
) -> Result<(), SnapshotError> {
    let mut rows = rows.peekable();
    if rows.peek().is_none() {
        return Ok(());
    }

    let options = DiskDb::options();
    let mut writer = SstFileWriter::create(&options);
    writer
        .open(path)
        .map_err(db_error("creating a rebuilt index SST file"))?;
    for (key, value) in rows {
        writer
            .put(key, value)
            .map_err(db_error("writing a rebuilt index SST file"))?;
    }
    writer
        .finish()
        .map_err(db_error("finishing a rebuilt index SST file"))?;

    super::ingest_paths(db, cf, &[path.to_path_buf()])
}
