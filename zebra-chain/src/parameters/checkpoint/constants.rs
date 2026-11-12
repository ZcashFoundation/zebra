//! Constants shared by some Zebra node services.

/// The expected maximum number of hashes in an `ObtainTips` or `ExtendTips` response.
///
/// Shared by synchronization and checkpoint admission so their pipeline bounds agree.
pub const MAX_TIPS_RESPONSE_HASH_COUNT: usize = 500;

/// The default number of blocks submitted concurrently for checkpoint verification.
///
/// Two tips-response batches keep several checkpoint ranges in the download and commit pipeline.
pub const DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT: usize = MAX_TIPS_RESPONSE_HASH_COUNT * 2;

/// We limit the maximum number of blocks in each checkpoint. Each block uses a
/// constant amount of memory for the supporting data structures and futures.
///
/// We choose a checkpoint gap that allows us to verify one checkpoint for
/// every `ObtainTips` or `ExtendTips` response.
///
/// `zcashd`'s maximum `FindBlocks` response size is 500 hashes. `zebrad` uses
/// 1 hash to verify the tip, and discards 1-2 hashes to work around `zcashd`
/// bugs. So the most efficient gap is slightly less than 500 blocks.
pub const MAX_CHECKPOINT_HEIGHT_GAP: usize = 400;

/// We limit the memory usage and download contention for each checkpoint,
/// based on the cumulative size of the serialized blocks in the chain.
///
/// Deserialized blocks (in memory) are slightly larger than serialized blocks
/// (on the network or disk). But they should be within a constant factor of the
/// serialized size.
pub const MAX_CHECKPOINT_BYTE_COUNT: u64 = 32 * 1024 * 1024;
