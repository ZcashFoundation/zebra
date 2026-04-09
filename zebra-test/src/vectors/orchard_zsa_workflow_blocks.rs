//! OrchardZSA workflow test blocks

#![allow(missing_docs)]

use hex::FromHex;
use lazy_static::lazy_static;

/// Represents a serialized block and its validity status.
pub struct OrchardWorkflowBlock {
    /// Block height.
    pub height: u32,
    /// Serialized byte data of the block.
    pub bytes: &'static [u8],
    /// Indicates whether the block is valid.
    pub is_valid: bool,
}

fn decode_bytes(hex: &str) -> Vec<u8> {
    <Vec<u8>>::from_hex(hex.trim()).expect("Block bytes are in valid hex representation")
}

lazy_static! {
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCK_1_BYTES: Vec<u8> =
        decode_bytes(include_str!("orchard-zsa-workflow-block-1.txt"));
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCK_2_BYTES: Vec<u8> =
        decode_bytes(include_str!("orchard-zsa-workflow-block-2.txt"));
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCK_3_BYTES: Vec<u8> =
        decode_bytes(include_str!("orchard-zsa-workflow-block-3.txt"));
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCK_4_BYTES: Vec<u8> =
        decode_bytes(include_str!("orchard-zsa-workflow-block-4.txt"));
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCK_5_BYTES: Vec<u8> =
        decode_bytes(include_str!("orchard-zsa-workflow-block-5.txt"));

    /// Test blocks for a Zcash Shielded Assets (ZSA) workflow.
    /// The sequence demonstrates issuing, transferring, and burning a custom
    /// asset, then finalizing the issuance and attempting an extra issue.
    ///
    /// The workflow blocks were generated using `zcash_tx_tool`
    pub static ref ORCHARD_ZSA_WORKFLOW_BLOCKS: Vec<OrchardWorkflowBlock> = vec![
        // Issue: 1000
        OrchardWorkflowBlock {
            height: 1,
            bytes: ORCHARD_ZSA_WORKFLOW_BLOCK_1_BYTES.as_slice(),
            is_valid: true
        },

        // Transfer
        OrchardWorkflowBlock {
            height: 2,
            bytes: ORCHARD_ZSA_WORKFLOW_BLOCK_2_BYTES.as_slice(),
            is_valid: true
        },

        // Burn: 7, Burn: 2
        OrchardWorkflowBlock {
            height: 3,
            bytes: ORCHARD_ZSA_WORKFLOW_BLOCK_3_BYTES.as_slice(),
            is_valid: true
        },

        // Issue: finalize
        OrchardWorkflowBlock {
            height: 4,
            bytes: ORCHARD_ZSA_WORKFLOW_BLOCK_4_BYTES.as_slice(),
            is_valid: true
        },

        // Try to issue: 2000
        OrchardWorkflowBlock {
            height: 5,
            bytes: ORCHARD_ZSA_WORKFLOW_BLOCK_5_BYTES.as_slice(),
            is_valid: false
        },
    ];
}
