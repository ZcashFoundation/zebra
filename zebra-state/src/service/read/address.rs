//! Reading address indexes.

pub mod balance;
pub mod tx_id;
pub mod utxo;

/// If the transparent address index queries are interrupted by a new finalized block,
/// retry this many times.
///
/// Once we're at the tip, we expect up to 2 blocks to arrive at the same time.
/// If any more arrive, the client should wait until we're synchronised with our peers.
const FINALIZED_ADDRESS_INDEX_RETRIES: usize = 3;
