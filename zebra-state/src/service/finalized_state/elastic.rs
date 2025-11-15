//! Elasticsearch representations of Zebra blocks and transactions.
//!
//! This module provides serializable structs for indexing Zcash blocks and transactions
//! in Elasticsearch. It extracts minimal information useful for search and analytics,
//! including block height, hash, timestamp, and basic transaction data.

use serde::Serialize;
use zebra_chain::{block::Block, transaction::Transaction};

/// A Zcash block for Elasticsearch indexing.
#[derive(Debug, Serialize)]
pub struct ElasticBlockObject {
    pub height: u32,
    pub hash: String,
    pub time: i64,
    pub tx_count: usize,
    pub transactions: Vec<ElasticTxObject>,
}

/// A Zcash transaction for Elasticsearch indexing.
#[derive(Debug, Serialize)]
pub struct ElasticTxObject {
    pub txid: String,
    pub version: u32,
    pub inputs: usize,
    pub outputs: usize,
}

impl ElasticBlockObject {
    /// Convert a Zebra `Block` into an `ElasticBlockObject`.
    pub fn from(block: &Block) -> Self {
        let height = block.coinbase_height().map(|h| h.0).unwrap_or(0);
        let hash = block.hash().to_string();
        let time = block.header.time.timestamp();

        let transactions = block
            .transactions
            .iter()
            .map(|tx| ElasticTxObject::from(tx.as_ref()))
            .collect();

        Self {
            height,
            hash,
            time,
            tx_count: block.transactions.len(),
            transactions,
        }
    }
}

impl ElasticTxObject {
    /// Convert a Zebra `Transaction` into an `ElasticTxObject`.
    pub fn from(tx: &Transaction) -> Self {
        let txid = tx.hash().to_string();

        let (version, inputs, outputs) = match tx {
            Transaction::V1 {
                inputs, outputs, ..
            } => (1, inputs.len(), outputs.len()),
            Transaction::V2 {
                inputs, outputs, ..
            } => (2, inputs.len(), outputs.len()),
            Transaction::V3 {
                inputs, outputs, ..
            } => (3, inputs.len(), outputs.len()),
            Transaction::V4 {
                inputs, outputs, ..
            } => (4, inputs.len(), outputs.len()),
            Transaction::V5 {
                inputs, outputs, ..
            } => (5, inputs.len(), outputs.len()),
            #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
            Transaction::V6 {
                inputs, outputs, ..
            } => (6, inputs.len(), outputs.len()),
        };

        Self {
            txid,
            version,
            inputs,
            outputs,
        }
    }
}
