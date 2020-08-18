//! Consensus check functions

use super::*;
use zebra_chain::block::Block;

/// Check that there is exactly one coinbase transaction in `Block`, and that
/// the coinbase transaction is the first transaction in the block.
///
/// "The first (and only the first) transaction in a block is a coinbase
/// transaction, which collects and spends any miner subsidy and transaction
/// fees paid by transactions included in this block." [§3.10][3.10]
///
/// [3.10]: https://zips.z.cash/protocol/protocol.pdf#coinbasetransactions
pub fn is_coinbase_first(block: &Block) -> Result<(), Error> {
    let first = block
        .transactions
        .get(0)
        .ok_or_else(|| "block has no transactions")?;
    let mut rest = block.transactions.iter().skip(1);
    if !first.is_coinbase() {
        return Err("first transaction must be coinbase".into());
    }
    if rest.any(|tx| tx.contains_coinbase_input()) {
        return Err("coinbase input found in non-coinbase transaction".into());
    }
    Ok(())
}
