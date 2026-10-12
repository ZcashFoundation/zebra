//! Tests for finalized database blocks and transactions.

#![allow(clippy::unwrap_in_result)]

mod snapshot;
mod vectors;

#[test]
fn parallel_spent_utxo_reads_match_serial_with_same_block_spends() {
    use std::collections::HashMap;

    use rayon::prelude::*;
    use zebra_chain::{
        amount::{Amount, NonNegative},
        block::Height,
        parameters::Network,
        transaction,
        transparent::{OrderedUtxo, OutPoint, Output, Script},
    };

    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{
            disk_db::{DiskWriteBatch, WriteDisk},
            disk_format::{OutputLocation, TransactionLocation},
            ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
        },
        Config,
    };

    let _init_guard = zebra_test::init();
    let db = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Network::Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .expect("an ephemeral database can be opened");
    let height = Height(7);
    let mut tx_hash_indexes = HashMap::new();
    let mut new_outputs = HashMap::new();
    let mut outpoints = Vec::new();
    let mut batch = DiskWriteBatch::new();
    for index in 0..32u8 {
        let outpoint = OutPoint {
            hash: transaction::Hash([index; 32]),
            index: 2,
        };
        let source_height = if index % 2 == 0 { Height(6) } else { height };
        let utxo = OrderedUtxo::new(
            Output::new(
                Amount::<NonNegative>::try_from(1_i64).unwrap(),
                Script::new(&[]),
            ),
            source_height,
            usize::from(index),
        );
        if source_height == height {
            tx_hash_indexes.insert(outpoint.hash, usize::from(index));
            new_outputs.insert(outpoint, utxo);
        } else {
            batch.zs_insert(
                &db.db.cf_handle("tx_loc_by_hash").unwrap(),
                outpoint.hash,
                TransactionLocation::from_usize(source_height, usize::from(index)),
            );
            batch.zs_insert(
                &db.db.cf_handle("utxo_by_out_loc").unwrap(),
                OutputLocation::from_usize(source_height, usize::from(index), 2),
                &utxo.utxo.output,
            );
        }
        outpoints.push(outpoint);
    }
    db.write_batch(batch).unwrap();

    let expected: Vec<_> = outpoints
        .iter()
        .map(|outpoint| {
            let location = db
                .output_location(outpoint)
                .unwrap_or_else(|| super::lookup_out_loc(height, outpoint, &tx_hash_indexes));
            let utxo = db
                .utxo(outpoint)
                .unwrap_or_else(|| new_outputs[outpoint].clone());
            (*outpoint, location, utxo.utxo)
        })
        .collect();
    let actual: Vec<_> = outpoints
        .par_iter()
        .map(|outpoint| {
            super::read_spent_utxo(&db, height, *outpoint, &tx_hash_indexes, &new_outputs)
        })
        .collect();
    assert_eq!(actual, expected);
}
