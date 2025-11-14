//! Validate coinbase transaction rewards as described in [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use crate::transparent::{self, Script};

/// Funding Streams functions apply for blocks at and after Canopy.
pub mod funding_streams;

/// Returns a new funding stream or lockbox disbursement coinbase output lock script, which pays to the P2SH `address`.
pub fn new_coinbase_script(address: &transparent::Address) -> Script {
    assert!(
        address.is_script_hash(),
        "incorrect coinbase script address: {address} \
         Funding streams and lockbox disbursements only \
         support transparent 'pay to script hash' (P2SH) addresses",
    );

    // > The “prescribed way” to pay a transparent P2SH address is to use a standard P2SH script
    // > of the form OP_HASH160 fs.RedeemScriptHash(height) OP_EQUAL as the scriptPubKey.
    //
    // [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
    address.script()
}
