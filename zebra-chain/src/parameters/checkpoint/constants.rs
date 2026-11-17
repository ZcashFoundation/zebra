//! Constants shared by some Zebra node services.

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
