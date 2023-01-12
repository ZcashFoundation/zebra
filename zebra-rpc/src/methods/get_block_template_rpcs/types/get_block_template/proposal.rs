//! `ProposalResponse` is the output of the `getblocktemplate` RPC method in 'proposal' mode.

use super::{GetBlockTemplate, Response};

/// Error response to a `getblocktemplate` RPC request in proposal mode.
///
/// See <https://en.bitcoin.it/wiki/BIP_0022#Appendix:_Example_Rejection_Reasons>
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalRejectReason {
    /// Block proposal rejected as invalid.
    Rejected,
}

/// Response to a `getblocktemplate` RPC request in proposal mode.
///
/// See <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged, rename_all = "kebab-case")]
pub enum ProposalResponse {
    /// Block proposal was rejected as invalid, returns `reject-reason` and server `capabilities`.
    ErrorResponse {
        /// Reason the proposal was invalid as-is.
        reject_reason: ProposalRejectReason,

        /// The getblocktemplate RPC capabilities supported by Zebra.
        capabilities: Vec<String>,
    },

    /// Block proposal was successfully validated, returns null.
    Valid,
}

impl From<ProposalRejectReason> for ProposalResponse {
    fn from(reject_reason: ProposalRejectReason) -> Self {
        Self::ErrorResponse {
            reject_reason,
            capabilities: GetBlockTemplate::capabilities(),
        }
    }
}

impl From<ProposalRejectReason> for Response {
    fn from(error_response: ProposalRejectReason) -> Self {
        Self::ProposalMode(ProposalResponse::from(error_response))
    }
}

impl From<ProposalResponse> for Response {
    fn from(proposal_response: ProposalResponse) -> Self {
        Self::ProposalMode(proposal_response)
    }
}

/// Returns a block proposal generated from a [`GetBlockTemplate`] RPC response.
pub fn proposal_block_from_template(
    GetBlockTemplate {
        version,
        height,
        previous_block_hash: GetBlockHash(previous_block_hash),
        default_roots:
            DefaultRoots {
                merkle_root,
                block_commitments_hash,
                ..
            },
        bits: difficulty_threshold,
        coinbase_txn,
        transactions: tx_templates,
        ..
    }: GetBlockTemplate,
) -> Result<Block, SerializationError> {
    if Height(height) > Height::MAX {
        Err(SerializationError::Parse(
            "height field must be lower than Height::MAX",
        ))?;
    };

    let mut transactions = vec![coinbase_txn.data.as_ref().zcash_deserialize_into()?];

    for tx_template in tx_templates {
        transactions.push(tx_template.data.as_ref().zcash_deserialize_into()?);
    }

    Ok(Block {
        header: Arc::new(block::Header {
            version,
            previous_block_hash,
            merkle_root,
            commitment_bytes: block_commitments_hash.into(),
            time: Utc::now(),
            difficulty_threshold,
            nonce: [0; 32],
            solution: Solution::default(),
        }),
        transactions,
    })
}
