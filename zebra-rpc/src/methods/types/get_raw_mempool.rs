//! Types used in `getrawmempool` RPC method.

use std::collections::HashMap;
use std::collections::HashSet;

use hex::ToHex as _;

use super::Zec;
use zebra_chain::transaction::VerifiedUnminedTx;
use zebra_chain::{amount::NonNegative, block::Height};
use zebra_node_services::mempool::TransactionDependencies;

/// Response to a `getrawmempool` RPC request.
///
/// See the notes for the [`Rpc::get_raw_mempool` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum GetRawMempool {
    /// The transaction IDs, as hex strings (verbose=0)
    TxIds(Vec<String>),
    /// A map of transaction IDs to mempool transaction details objects
    /// (verbose=1)
    Verbose(HashMap<String, MempoolObject>),
}

/// A mempool transaction details object as returned by `getrawmempool` in
/// verbose mode.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct MempoolObject {
    /// Transaction size in bytes.
    pub(crate) size: u64,
    /// Transaction fee in zatoshi.
    pub(crate) fee: Zec<NonNegative>,
    /// Transaction fee with fee deltas used for mining priority.
    #[serde(rename = "modifiedfee")]
    pub(crate) modified_fee: Zec<NonNegative>,
    /// Local time transaction entered pool in seconds since 1 Jan 1970 GMT
    pub(crate) time: i64,
    /// Block height when transaction entered pool.
    pub(crate) height: Height,
    /// Number of in-mempool descendant transactions (including this one).
    pub(crate) descendantcount: u64,
    /// Size of in-mempool descendants (including this one).
    pub(crate) descendantsize: u64,
    /// Modified fees (see "modifiedfee" above) of in-mempool descendants
    /// (including this one).
    pub(crate) descendantfees: u64,
    /// Transaction IDs of unconfirmed transactions used as inputs for this
    /// transaction.
    pub(crate) depends: Vec<String>,
}

impl MempoolObject {
    pub(crate) fn from_verified_unmined_tx(
        unmined_tx: &VerifiedUnminedTx,
        transactions: &[VerifiedUnminedTx],
        transaction_dependencies: &TransactionDependencies,
    ) -> Self {
        // Map transactions by their txids to make lookups easier
        let transactions_by_id = transactions
            .iter()
            .map(|unmined_tx| (unmined_tx.transaction.id.mined_id(), unmined_tx))
            .collect::<HashMap<_, _>>();

        // Get txids of this transaction's descendants (dependents)
        let empty_set = HashSet::new();
        let deps = transaction_dependencies
            .dependents()
            .get(&unmined_tx.transaction.id.mined_id())
            .unwrap_or(&empty_set);
        let deps_len = deps.len();

        // For each dependent: get the tx, then its size and fee; then sum them
        // up
        let (deps_size, deps_fees) = deps
            .iter()
            .filter_map(|id| transactions_by_id.get(id))
            .map(|unmined_tx| (unmined_tx.transaction.size, unmined_tx.miner_fee))
            .reduce(|(size1, fee1), (size2, fee2)| {
                (size1 + size2, (fee1 + fee2).unwrap_or_default())
            })
            .unwrap_or((0, Default::default()));

        // Create the MempoolObject from the information we have gathered
        let mempool_object = MempoolObject {
            size: unmined_tx.transaction.size as u64,
            fee: unmined_tx.miner_fee.into(),
            // Change this if we ever support fee deltas (prioritisetransaction call)
            modified_fee: unmined_tx.miner_fee.into(),
            time: unmined_tx
                .time
                .map(|time| time.timestamp())
                .unwrap_or_default(),
            height: unmined_tx.height.unwrap_or(Height(0)),
            // Note that the following three count this transaction itself
            descendantcount: deps_len as u64 + 1,
            descendantsize: (deps_size + unmined_tx.transaction.size) as u64,
            descendantfees: (deps_fees + unmined_tx.miner_fee)
                .unwrap_or_default()
                .into(),
            // Get dependencies as a txid vector
            depends: transaction_dependencies
                .dependencies()
                .get(&unmined_tx.transaction.id.mined_id())
                .cloned()
                .unwrap_or_else(HashSet::new)
                .iter()
                .map(|id| id.encode_hex())
                .collect(),
        };
        mempool_object
    }
}
