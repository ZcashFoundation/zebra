//! OrchardZSA workflow test blocks

#![allow(missing_docs)]

use hex::FromHex;
use lazy_static::lazy_static;

/// Represents a serialized block and its validity status.
pub struct OrchardWorkflowBlock {
    /// Serialized byte data of the block.
    pub bytes: Vec<u8>,
    /// Indicates whether the block is valid.
    pub is_valid: bool,
}

fn decode_bytes(hex: &str) -> Vec<u8> {
    <Vec<u8>>::from_hex((hex).trim()).expect("Block bytes are in valid hex representation")
}

lazy_static! {
    /// Test blocks for a Zcash Shielded Assets (ZSA) workflow.
    /// The sequence demonstrates issuing, transferring and burning a custom
    /// asset, then finalising the issuance and attempting an extra issue.
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCKS: Vec<OrchardWorkflowBlock> = vec![
        // Issue: 1000
        OrchardWorkflowBlock {
            bytes: decode_bytes(include_str!("orchard-zsa-workflow-blocks-1.txt")),
            is_valid: true
        },

        // Transfer
        OrchardWorkflowBlock {
            bytes: decode_bytes(include_str!("orchard-zsa-workflow-blocks-2.txt")),
            is_valid: true
        },

        // Burn: 7, Burn: 2
        OrchardWorkflowBlock {
            bytes: decode_bytes(include_str!("orchard-zsa-workflow-blocks-3.txt")),
            is_valid: true
        },

        // Issue: finalize
        OrchardWorkflowBlock {
            bytes: decode_bytes(include_str!("orchard-zsa-workflow-blocks-4.txt")),
            is_valid: true
        },

        // Try to issue: 2000
        OrchardWorkflowBlock {
            bytes: decode_bytes(include_str!("orchard-zsa-workflow-blocks-5.txt")),
            is_valid: false
        },
    ];
}
