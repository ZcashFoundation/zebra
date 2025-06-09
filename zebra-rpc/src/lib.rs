//! A Zebra Remote Procedure Call (RPC) interface

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_rpc")]

pub mod config;
pub mod methods;
pub mod queue;
pub mod server;
pub mod sync;

#[cfg(feature = "indexer-rpcs")]
pub mod indexer;

#[cfg(test)]
mod tests;

pub use methods::types::{
    get_block_template::{
        fetch_state_tip_and_local_time, generate_coinbase_and_roots,
        proposal::proposal_block_from_template,
    },
    submit_block::SubmitBlockChannel,
};

/// Types, constants, and functions needed by clients of Zebra's RPC server
//
// Re-exports types to build the API. We want to flatten the module substructure
// which is split mostly to keep file sizes small. Additionally, we re-export
// types from other crates which are exposed in the API.
//
// TODO: Move `hex_data` and `trees` modules to/under the `types` module?
pub mod client {
    pub use zebra_chain;

    pub use crate::methods::{
        hex_data::HexData,
        trees::{
            Commitments, GetSubtreesByIndexResponse, GetTreestateResponse, SubtreeRpcData,
            Treestate,
        },
        types::{
            default_roots::DefaultRoots,
            get_block_template::{
                BlockProposalResponse, BlockTemplateResponse, BlockTemplateTimeSource,
                GetBlockTemplateCapability, GetBlockTemplateParameters,
                GetBlockTemplateRequestMode, GetBlockTemplateResponse,
            },
            get_mining_info::GetMiningInfoResponse,
            get_raw_mempool::{GetRawMempoolResponse, MempoolObject},
            peer_info::{GetPeerInfoResponse, PeerInfo},
            submit_block::{SubmitBlockErrorResponse, SubmitBlockResponse},
            subsidy::{BlockSubsidy, FundingStream, GetBlockSubsidyResponse},
            transaction::{
                Input, Orchard, OrchardAction, Output, ScriptPubKey, ScriptSig, ShieldedOutput,
                ShieldedSpend, TransactionObject, TransactionTemplate,
            },
            unified_address::ZListUnifiedReceiversResponse,
            validate_address::ValidateAddressResponse,
            z_validate_address::{ZValidateAddressResponse, ZValidateAddressType},
        },
        BlockHeaderObject, BlockObject, GetAddressBalanceRequest, GetAddressBalanceResponse,
        GetAddressTxIdsRequest, GetAddressUtxosResponse, GetBlockHashResponse,
        GetBlockHeaderResponse, GetBlockHeightAndHashResponse, GetBlockResponse,
        GetBlockTransaction, GetBlockTrees, GetBlockchainInfoResponse, GetInfoResponse,
        GetRawTransactionResponse, Hash, SendRawTransactionResponse, Utxo,
    };

    /// Constants needed by clients of Zebra's RPC server
    // TODO: Export all other constants?
    pub use crate::methods::types::long_poll::LONG_POLL_ID_LENGTH;
}
