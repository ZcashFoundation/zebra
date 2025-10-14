//! Types used in `getmempoolinfo` RPC method.

/// Response to a `getmempoolinfo` RPC request.
///
/// See the notes for the [`Rpc::get_mempool_info` method].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GetMempoolInfoResponse {
    /// Current tx count
    pub size: usize,
    /// Sum of all tx sizes
    pub bytes: usize,
    /// Total memory usage for the mempool
    pub usage: usize,
    /// Whether the node has finished notifying all listeners/tests about every transaction currently in the mempool.
    /// This key is returned only when the node is running in regtest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fully_notified: Option<bool>,
}
