//! OrchardZSA test vectors

use hex::FromHex;
use lazy_static::lazy_static;

lazy_static! {
/// Test blocks for a Zcash Shielded Assets (ZSA) workflow.
/// The sequence demonstrates issuing, transferring and burning a custom
/// asset, then finalising the issuance and attempting an extra issue.
pub static ref ORCHARD_ZSA_WORKFLOW_BLOCKS: [Vec<u8>; 5] = [
        // Issue: 1000
        include_str!("orchard-zsa-workflow-block-1.txt").trim(),
        // Transfer
        include_str!("orchard-zsa-workflow-block-2.txt").trim(),
        // Burn: 7, Burn: 2
        include_str!("orchard-zsa-workflow-block-3.txt").trim(),
        // Issue: finalize
        include_str!("orchard-zsa-workflow-block-4.txt").trim(),
        // Try to issue: 2000
        include_str!("orchard-zsa-workflow-block-5.txt").trim(),
    ]
    .map(|hex| <Vec<u8>>::from_hex(hex).expect("Block bytes are in valid hex representation"));
}
